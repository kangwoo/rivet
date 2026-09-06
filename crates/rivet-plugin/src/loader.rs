//! The loader: `discover → validate → load → register → active`.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;
use rivet_core::capability::{CapabilityVersion, Permission, PermissionSet};
use rivet_core::error::Error;
use rivet_core::event::{Event, EventBus, EventEnvelope, PluginEvent};
use rivet_core::id::{PluginId, PluginInstanceId};
use rivet_core::plugin::{Plugin, PluginContext, PluginManifest, PluginState};
use rivet_runtime::Registry;
use rivet_runtime::registry::Owner;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::guard::GuardedRegistry;
use crate::source::{Origin, PluginSource};

/// What the loader knows about one plugin.
#[derive(Clone, Debug)]
pub struct PluginRecord {
    pub id: PluginId,
    pub manifest: PluginManifest,
    pub origin: Origin,
    pub state: PluginState,
    /// The live instance, when there is one.
    pub instance_id: Option<PluginInstanceId>,
    /// `manifest ∩ profile`, computed at `VALIDATED`.
    pub effective: PermissionSet,
    /// Permissions the manifest asked for that the profile removed entirely. Drives the
    /// "removed by profile" column of `rivet plugin show`.
    pub denied: Vec<Permission>,
    /// Observed through the guard, not self-reported. e.g. `["tool:read_file", …]`.
    pub registered: Vec<String>,
    /// What the plugin's own `PluginHandle` claimed. Kept so `rivet doctor` can report a
    /// plugin whose claim and registrations disagree.
    pub claimed: Vec<String>,
    /// Retained after `FAILED` so `rivet plugin list` can still say why.
    pub error: Option<String>,
}

impl PluginRecord {
    /// Whether the plugin's `PluginHandle` matched what it actually registered.
    ///
    /// A handle is a claim; the guard's list is the fact. They diverge when a plugin
    /// overstates what it registered — exactly what the guard exists to catch.
    #[must_use]
    pub fn claim_matches_reality(&self) -> bool {
        let mut claimed = self.claimed.clone();
        let mut registered = self.registered.clone();
        claimed.sort();
        registered.sort();
        claimed == registered
    }
}

/// The outcome of loading a batch.
#[derive(Debug, Default)]
pub struct LoadReport {
    pub loaded: Vec<PluginId>,
    /// Every failure, not just the first: an operator with a five-plugin config should not
    /// have to fix them one run at a time.
    pub failed: Vec<(PluginId, Error)>,
    /// Everything the batch registered, in load order.
    pub registered: Vec<String>,
}

/// One loaded plugin instance, kept so `unload` can reverse exactly what `load` did.
#[derive(Debug)]
struct Instance {
    plugin: Arc<dyn Plugin>,
    ctx: PluginContext,
    /// A child of the loader's root token, so unloading one plugin does not stop another
    /// plugin's background work.
    token: CancellationToken,
}

/// Discovers, validates, loads and unloads plugins against one [`Registry`].
#[derive(Debug)]
pub struct PluginLoader {
    registry: Registry,
    events: Arc<dyn EventBus>,
    host_abi: CapabilityVersion,
    profile_grant: PermissionSet,
    root: CancellationToken,
    records: Vec<PluginRecord>,
    sources: HashMap<PluginId, PluginSource>,
    instances: HashMap<PluginId, Instance>,
}

impl PluginLoader {
    #[must_use]
    pub fn new(
        registry: Registry,
        events: Arc<dyn EventBus>,
        host_abi: CapabilityVersion,
        profile_grant: PermissionSet,
    ) -> Self {
        Self {
            registry,
            events,
            host_abi,
            profile_grant,
            root: CancellationToken::new(),
            records: Vec::new(),
            sources: HashMap::new(),
            instances: HashMap::new(),
        }
    }

    /// Parse every source's manifest. Leaves each record `DISCOVERED`.
    ///
    /// # Errors
    /// A manifest that does not parse, or two sources declaring the same id — both are
    /// mistakes in the host's catalog rather than in an operator's config, so both are
    /// fatal rather than recorded. A record is keyed by [`PluginId`], which a manifest
    /// that failed to parse does not have.
    pub fn discover(&mut self, sources: &[PluginSource]) -> rivet_core::Result<()> {
        let mut discovered = Vec::new();
        for source in sources {
            let manifest = source.manifest()?;
            if let Some(existing) = self.sources.get(&manifest.id) {
                return Err(Error::plugin(format!(
                    "plugin id `{}` is declared twice: by {} and by {}",
                    manifest.id, existing.origin, source.origin
                )));
            }
            let id = manifest.id.clone();
            self.sources.insert(id.clone(), source.clone());
            self.records.push(PluginRecord {
                id: id.clone(),
                manifest,
                origin: source.origin.clone(),
                state: PluginState::Discovered,
                instance_id: None,
                effective: PermissionSet::empty(),
                denied: Vec::new(),
                registered: Vec::new(),
                claimed: Vec::new(),
                error: None,
            });
            discovered.push(id);
        }
        for plugin_id in discovered {
            self.publish(PluginEvent::Discovered { plugin_id });
        }
        Ok(())
    }

    /// Check the ABI and compute `manifest ∩ profile`. Nothing is constructed here, which
    /// is what makes an incompatible plugin unable to register anything (2.3).
    ///
    /// The ABI is the only thing that can fail: an unmeetable permission is not an error
    /// but an input to the plugin's own decision at `load`, and a scope that could not be
    /// met was already rejected when the manifest was parsed.
    pub fn validate(&mut self) {
        let host_abi = self.host_abi;
        let grant = self.profile_grant.clone();
        let mut rejected = Vec::new();

        for record in &mut self.records {
            if record.state != PluginState::Discovered {
                continue;
            }
            if !record.manifest.is_compatible_with(host_abi) {
                let error = format!(
                    "`{}` was built against ABI {}, and this host implements {}",
                    record.id, record.manifest.abi_version, host_abi
                );
                record.state = PluginState::Failed;
                record.error = Some(error.clone());
                rejected.push((record.id.clone(), error));
                continue;
            }
            record.effective = record.manifest.requested_permissions().intersect(&grant);
            record.denied = record
                .manifest
                .permissions
                .iter()
                .filter(|wanted| meet_with(wanted, &grant).is_none())
                .cloned()
                .collect();
            record.state = PluginState::Validated;
        }

        for (plugin_id, error) in rejected {
            self.publish(PluginEvent::LoadFailed { plugin_id, error });
        }
    }

    /// Construct one plugin and run its `load` through the guard.
    ///
    /// # Errors
    /// An unknown id, a record in a state that cannot be loaded, or anything the plugin's
    /// own `load` returns — including a panic. On failure the instance's registrations are
    /// removed and its cancellation token is cancelled, so a partially registered plugin
    /// leaves nothing behind (2.4).
    pub async fn load(&mut self, id: &PluginId, config: Value) -> rivet_core::Result<()> {
        let index = self.loadable(id)?;

        let source = self
            .sources
            .get(id)
            .expect("every record was created from a source")
            .clone();
        let manifest = self.records[index].manifest.clone();
        let plugin = source.construct(manifest.clone());
        let instance_id = PluginInstanceId::new();
        let token = self.root.child_token();
        let guard = Arc::new(GuardedRegistry::new(
            self.registry.scoped(Owner {
                plugin_id: id.clone(),
                instance_id,
            }),
            id.clone(),
            manifest.capabilities.clone(),
        ));
        let ctx = PluginContext {
            instance_id,
            manifest,
            permissions: self.records[index].effective.clone(),
            registry: guard.clone(),
            events: self.events.clone(),
            shutdown: token.clone(),
            config,
        };

        // A panicking plugin takes the same path as a failing one. Caveat: this catches a
        // panic in `load` itself, not one in a task the plugin spawned.
        let outcome = AssertUnwindSafe(plugin.load(ctx.clone()))
            .catch_unwind()
            .await;
        let result = match outcome {
            Ok(result) => result,
            Err(payload) => Err(Error::plugin(format!(
                "`{id}` panicked during load: {}",
                panic_message(&payload)
            ))),
        };

        match result {
            Ok(handle) => {
                let registered = guard.observed();
                {
                    let record = &mut self.records[index];
                    record.state = PluginState::Loaded;
                    record.instance_id = Some(instance_id);
                    record.registered.clone_from(&registered);
                    record.claimed = handle.registered;
                    record.error = None;
                }
                self.instances
                    .insert(id.clone(), Instance { plugin, ctx, token });
                self.publish(PluginEvent::Loaded {
                    plugin_id: id.clone(),
                    capabilities: registered,
                });
                Ok(())
            }
            Err(error) => {
                self.registry.unregister_all(instance_id).await;
                // The token goes with the registrations: background work the plugin armed
                // before it failed would otherwise outlive the failure.
                token.cancel();
                {
                    let record = &mut self.records[index];
                    record.state = PluginState::Failed;
                    record.instance_id = None;
                    record.registered.clear();
                    record.claimed.clear();
                    record.error = Some(error.to_string());
                }
                self.publish(PluginEvent::LoadFailed {
                    plugin_id: id.clone(),
                    error: error.to_string(),
                });
                Err(error)
            }
        }
    }

    /// Load a batch, attempting every id and collecting every failure.
    ///
    /// Duplicate ids load once, in first-seen order. Order is not a contract: the registry
    /// rejects name collisions and interceptors are sorted by priority, so nothing
    /// downstream depends on it.
    ///
    /// The batch commits as a whole. Records are promoted from `LOADED` to `ACTIVE` only
    /// when nothing failed, so a record still at `LOADED` after this returns is visibly
    /// one the host is about to tear down.
    pub async fn load_selected(
        &mut self,
        ids: &[PluginId],
        config_for: &dyn Fn(&PluginId) -> Value,
    ) -> LoadReport {
        let mut report = LoadReport::default();
        let mut seen: Vec<PluginId> = Vec::new();
        for id in ids {
            if seen.contains(id) {
                continue;
            }
            seen.push(id.clone());
            match self.load(id, config_for(id)).await {
                Ok(()) => {
                    report.loaded.push(id.clone());
                    if let Some(record) = self.record(id) {
                        report.registered.extend(record.registered.iter().cloned());
                    }
                }
                Err(error) => report.failed.push((id.clone(), error)),
            }
        }

        if report.failed.is_empty() {
            for record in &mut self.records {
                if record.state == PluginState::Loaded {
                    record.state = PluginState::Active;
                }
            }
        }
        report
    }

    /// Unregister a plugin's capabilities, cancel its token, then let it stop its own work.
    ///
    /// # Errors
    /// An id that is not loaded, or whatever `Plugin::unload` returns. An error from
    /// `unload` is retained on the record and returned, but the state is still `UNLOADED`:
    /// the capabilities are already gone, and reporting `FAILED` would suggest the
    /// registry is dirty when it is not.
    pub async fn unload(&mut self, id: &PluginId) -> rivet_core::Result<()> {
        let Some(instance) = self.instances.remove(id) else {
            return Err(Error::not_found(format!("`{id}` is not loaded")));
        };
        self.set_state(id, PluginState::Unloading);
        self.registry.unregister_all(instance.ctx.instance_id).await;
        instance.token.cancel();
        let result = instance.plugin.unload(instance.ctx.clone()).await;

        if let Some(record) = self.records.iter_mut().find(|r| &r.id == id) {
            record.state = PluginState::Unloaded;
            record.instance_id = None;
            record.registered.clear();
            record.claimed.clear();
            record.error = result.as_ref().err().map(ToString::to_string);
        }
        self.publish(PluginEvent::Unloaded {
            plugin_id: id.clone(),
        });
        result
    }

    /// Unload everything, in reverse load order. Errors are reported, not propagated.
    pub async fn unload_all(&mut self) {
        let ids: Vec<PluginId> = self
            .records
            .iter()
            .rev()
            .filter(|record| self.instances.contains_key(&record.id))
            .map(|record| record.id.clone())
            .collect();
        for id in ids {
            if let Err(error) = self.unload(&id).await {
                tracing::warn!(plugin = %id, %error, "plugin unload reported an error");
            }
        }
    }

    /// Cancel every plugin's shutdown token without unloading. The Ctrl-C path.
    pub fn shutdown(&self) {
        self.root.cancel();
    }

    #[must_use]
    pub fn records(&self) -> &[PluginRecord] {
        &self.records
    }

    #[must_use]
    pub fn record(&self, id: &PluginId) -> Option<&PluginRecord> {
        self.records.iter().find(|record| &record.id == id)
    }

    /// The ids this build provides, comma-separated, for error messages.
    #[must_use]
    pub fn known_ids(&self) -> String {
        self.records
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The record's index, if `id` is known and in a state that can be loaded.
    fn loadable(&self, id: &PluginId) -> rivet_core::Result<usize> {
        let index = self.index_of(id).ok_or_else(|| {
            Error::not_found(format!(
                "no plugin `{id}` was discovered; this build provides {}",
                self.known_ids()
            ))
        })?;
        let record = &self.records[index];
        match record.state {
            // `UNLOADED` is loadable again: that is hot reload, and it mints a fresh
            // instance id rather than reviving the old one.
            PluginState::Validated | PluginState::Unloaded => Ok(index),
            PluginState::Discovered => Err(Error::plugin(format!(
                "`{id}` has not been validated yet; call `validate` first"
            ))),
            PluginState::Loaded | PluginState::Active | PluginState::Unloading => {
                Err(Error::plugin(format!(
                    "`{id}` is already {}; reloading is `unload` then `load`, never a \
                     second instance",
                    state_label(record.state)
                )))
            }
            PluginState::Failed => Err(Error::plugin(format!(
                "`{id}` failed and is not retried in this process: {}",
                record.error.as_deref().unwrap_or("no reason recorded")
            ))),
        }
    }

    fn index_of(&self, id: &PluginId) -> Option<usize> {
        self.records.iter().position(|record| &record.id == id)
    }

    fn set_state(&mut self, id: &PluginId, state: PluginState) {
        if let Some(record) = self.records.iter_mut().find(|r| &r.id == id) {
            record.state = state;
        }
    }

    fn publish(&self, event: PluginEvent) {
        self.events
            .publish(EventEnvelope::new(Event::Plugin(event)));
    }
}

/// The best `wanted` can become under `grant`, or `None` when the grant removes it.
///
/// This is [`PermissionSet::intersect`] for a single permission, kept so a record can say
/// *which* request the profile removed rather than only that the set got smaller.
fn meet_with(wanted: &Permission, grant: &PermissionSet) -> Option<Permission> {
    grant.granted().iter().find_map(|held| wanted.meet(held))
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "a non-string panic payload".to_string())
        },
        |text| (*text).to_string(),
    )
}

/// The state's name as `rivet plugin list` prints it.
#[must_use]
pub const fn state_label(state: PluginState) -> &'static str {
    match state {
        PluginState::Discovered => "DISCOVERED",
        PluginState::Validated => "VALIDATED",
        PluginState::Loaded => "LOADED",
        PluginState::Active => "ACTIVE",
        PluginState::Unloading => "UNLOADING",
        PluginState::Unloaded => "UNLOADED",
        PluginState::Failed => "FAILED",
    }
}
