//! The plugin contract.
//!
//! A plugin does one thing: **register capabilities into registries it was handed.** It
//! does not reach into the runtime, mutate state, or hold a reference to the agent loop.
//! That constraint is what makes the same trait implementable in-process today and over
//! RPC or WASM later without changing plugin code.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::capability::{CapabilityKind, CapabilityVersion, Permission, PermissionSet};
use crate::context::ContextProvider;
use crate::event::{EventBus, EventSubscriber};
use crate::id::{PluginId, PluginInstanceId};
use crate::memory::Memory;
use crate::model::Model;
use crate::policy::{Policy, PolicyRequest, RestrictiveDecision};
use crate::sandbox::Sandbox;
use crate::task::{Scheduler, Workflow};
use crate::tool::Tool;

/// Declarative metadata, mirroring `rivet-plugin.toml`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: PluginId,
    pub name: String,
    /// Plugin's own version, independent of the contract version.
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Contract version this plugin was built against. The host refuses to load a plugin
    /// whose version it cannot satisfy — see [`CapabilityVersion::accepts`].
    pub abi_version: CapabilityVersion,
    /// Slots this plugin intends to fill. Registering into a slot not declared here is a
    /// contract violation and the loader rejects it.
    #[serde(default)]
    pub capabilities: Vec<CapabilityKind>,
    /// Permissions requested. The effective grant is this intersected with the profile.
    #[serde(default)]
    pub permissions: Vec<Permission>,
}

impl PluginManifest {
    pub fn new(id: PluginId, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            version: version.into(),
            description: String::new(),
            abi_version: crate::ABI_VERSION,
            capabilities: Vec::new(),
            permissions: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_capabilities(mut self, caps: impl IntoIterator<Item = CapabilityKind>) -> Self {
        self.capabilities = caps.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_permissions(mut self, perms: impl IntoIterator<Item = Permission>) -> Self {
        self.permissions = perms.into_iter().collect();
        self
    }

    #[must_use]
    pub fn requested_permissions(&self) -> PermissionSet {
        PermissionSet::new(self.permissions.clone())
    }

    /// Whether a host implementing `host_abi` can load this plugin.
    #[must_use]
    pub fn is_compatible_with(&self, host_abi: CapabilityVersion) -> bool {
        host_abi.accepts(self.abi_version)
    }
}

/// Registration surface handed to a plugin at load.
///
/// Every method is fallible and every registration is *scoped to the plugin*: the
/// registry records who registered what, so `unload` can reverse exactly that set. A
/// plugin cannot unregister another plugin's capability.
#[async_trait]
pub trait PluginRegistry: Send + Sync + fmt::Debug {
    async fn register_model(&self, model: Arc<dyn Model>) -> crate::Result<()>;
    async fn register_tool(&self, tool: Arc<dyn Tool>) -> crate::Result<()>;
    async fn register_context_provider(
        &self,
        provider: Arc<dyn ContextProvider>,
    ) -> crate::Result<()>;
    async fn register_policy(&self, policy: Arc<dyn Policy>) -> crate::Result<()>;
    async fn register_sandbox(&self, sandbox: Arc<dyn Sandbox>) -> crate::Result<()>;
    async fn register_memory(&self, memory: Arc<dyn Memory>) -> crate::Result<()>;
    async fn register_workflow(&self, workflow: Arc<dyn Workflow>) -> crate::Result<()>;
    async fn register_scheduler(&self, scheduler: Arc<dyn Scheduler>) -> crate::Result<()>;
    async fn register_subscriber(&self, subscriber: Arc<dyn EventSubscriber>) -> crate::Result<()>;
    async fn register_interceptor(&self, interceptor: Arc<dyn Interceptor>) -> crate::Result<()>;
    async fn register_session_store(
        &self,
        store: Arc<dyn crate::session::SessionStore>,
    ) -> crate::Result<()>;
    async fn register_evaluator(
        &self,
        evaluator: Arc<dyn crate::memory::Evaluator>,
    ) -> crate::Result<()>;
}

/// What a plugin gets at load time.
///
/// Note the absence of a runtime handle, a session store, and a task graph. A plugin that
/// needs to observe the run subscribes to events; a plugin that needs to *change* the run
/// registers an [`Interceptor`] or a [`Policy`]. There is no third way in.
#[derive(Clone)]
pub struct PluginContext {
    pub instance_id: PluginInstanceId,
    pub manifest: PluginManifest,
    /// The effective grant after intersecting the manifest with the active profile. A
    /// plugin should check this and fail loudly at load if something it needs was denied,
    /// rather than failing at the first tool call.
    pub permissions: PermissionSet,
    pub registry: Arc<dyn PluginRegistry>,
    pub events: Arc<dyn EventBus>,
    /// Cancelled when the runtime shuts down or this plugin is unloaded.
    ///
    /// A plugin that spawns background work must select on this. Without it the advice in
    /// `docs/plugin.md` ("clean up on the error path") would have no handle to clean up
    /// *with*, and every unload would leak a task.
    pub shutdown: tokio_util::sync::CancellationToken,
    /// Plugin-specific config from the `[plugins.<id>]` table.
    pub config: serde_json::Value,
}

impl fmt::Debug for PluginContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginContext")
            .field("instance_id", &self.instance_id)
            .field("plugin_id", &self.manifest.id)
            .field("permissions", &self.permissions)
            .finish_non_exhaustive()
    }
}

/// Returned by a successful load. Dropping it must not unregister anything — teardown
/// goes through [`Plugin::unload`] so failures are observable.
#[derive(Debug, Default)]
pub struct PluginHandle {
    /// Human-readable list of what got registered, for `rivet plugin list`.
    pub registered: Vec<String>,
}

impl PluginHandle {
    #[must_use]
    pub fn new(registered: impl IntoIterator<Item = String>) -> Self {
        Self {
            registered: registered.into_iter().collect(),
        }
    }
}

/// An extension unit.
#[async_trait]
pub trait Plugin: Send + Sync + fmt::Debug {
    fn manifest(&self) -> PluginManifest;

    /// Register capabilities.
    ///
    /// Must be idempotent-safe on failure: if `load` returns `Err` after partial
    /// registration, the loader rolls back everything this instance registered. Do not
    /// leave background tasks running on the error path.
    async fn load(&self, ctx: PluginContext) -> crate::Result<PluginHandle>;

    /// Release resources. The loader has already unregistered this plugin's capabilities
    /// by the time this runs, so `unload` only needs to stop what the plugin itself
    /// started.
    async fn unload(&self, ctx: PluginContext) -> crate::Result<()>;
}

/// The *only* way a plugin changes what the runtime does, short of a policy.
///
/// Separate from [`EventSubscriber`] on purpose: subscribers observe and cannot block,
/// interceptors can block and are therefore enumerated, ordered, and time-limited by the
/// runtime. If everything could intercept, "what can stop my tool call" would be
/// unanswerable.
#[async_trait]
pub trait Interceptor: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;

    /// Evaluation order. Lower runs first; ties break on `name` so the order is a
    /// property of configuration rather than of registration timing.
    ///
    /// Order affects only which *reason* the user sees first. It cannot affect the
    /// outcome, because results are folded with
    /// [`crate::policy::PolicyDecision::combine`] rather than short-circuiting.
    fn priority(&self) -> i32 {
        0
    }

    /// Runs before the policy chain. Returning `Some` contributes a restriction;
    /// returning `None` abstains.
    ///
    /// Note the return type: an interceptor **cannot** answer "allow". It may only make
    /// the outcome stricter. Its result is folded into the same chain as every policy, so
    /// a permissive interceptor cannot override a restrictive policy — and neither can it
    /// win by being registered first.
    ///
    /// The runtime applies a timeout; an interceptor that hangs is treated as `None` and
    /// reported.
    async fn before_tool_call(
        &self,
        _request: &PolicyRequest,
    ) -> crate::Result<Option<RestrictiveDecision>> {
        Ok(None)
    }
}

/// Lifecycle of a plugin instance, as tracked by the loader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PluginState {
    Discovered,
    /// Manifest parsed, ABI checked, permissions resolved.
    Validated,
    Loaded,
    Active,
    Unloading,
    Unloaded,
    /// Terminal. The error is retained for `rivet plugin list`.
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::FsScope;

    fn manifest() -> PluginManifest {
        PluginManifest::new(PluginId::new("rivet.tool-git").unwrap(), "Git", "0.1.0")
            .with_capabilities([CapabilityKind::Tool])
            .with_permissions([
                Permission::FsRead(FsScope::Workspace),
                Permission::ProcessSpawn,
            ])
    }

    #[test]
    fn a_plugin_built_against_a_different_abi_is_rejected() {
        let mut m = manifest();
        m.abi_version = CapabilityVersion::new(0, 99);
        assert!(!m.is_compatible_with(crate::ABI_VERSION));
    }

    #[test]
    fn a_matching_abi_is_accepted() {
        assert!(manifest().is_compatible_with(crate::ABI_VERSION));
    }

    #[test]
    fn requested_permissions_are_deduplicated_and_ordered() {
        let mut m = manifest();
        m.permissions.push(Permission::ProcessSpawn);
        let set = m.requested_permissions();
        assert_eq!(set.granted().len(), 2, "duplicates collapse");
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let m = manifest();
        let json = serde_json::to_string(&m).unwrap();
        let back: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, m.id);
        assert_eq!(back.capabilities, m.capabilities);
        assert_eq!(back.permissions, m.permissions);
    }
}
