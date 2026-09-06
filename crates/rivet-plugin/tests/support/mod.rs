//! Scripted plugins for driving the loader.
//!
//! `PluginSource::construct` is a plain `fn` pointer — deliberately, because a closure
//! capturing host state is exactly what cannot survive the Phase 6 process boundary. So a
//! test that needs to watch what a plugin did registers its script in a process-wide table
//! first, keyed by plugin id, and the construct function looks it up. Every test therefore
//! uses ids of its own.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use async_trait::async_trait;
use rivet_core::capability::PermissionSet;
use rivet_core::error::Error;
use rivet_core::id::PluginId;
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
use rivet_core::policy::{Policy, PolicyDecision, PolicyRequest};
use rivet_core::tool::{Tool, ToolContext, ToolResult, ToolSpec};
use rivet_plugin::{PluginLoader, PluginSource};
use rivet_runtime::{BroadcastBus, Registry};
use tokio_util::sync::CancellationToken;

/// What a scripted plugin does once it has registered its tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum After {
    #[default]
    Succeed,
    /// Return `Err` — the partial-registration case.
    Fail,
    Panic,
    /// Register a policy the manifest never declared, to trip the guard.
    RegisterUndeclaredPolicy,
    /// Hand a spawned task the registry, return `Err`, and let the task register *after*
    /// `load` has returned and the loader has rolled the instance back. This is the shape
    /// of "register once the connection is up", which is how a plugin with a backend
    /// naturally wants to be written.
    FailAfterArmingALateRegistration,
    /// Register a tool from `unload`, after the loader's `unregister_all` has run.
    RegisterFromUnload,
}

/// One plugin's script, and what it recorded.
#[derive(Debug)]
pub struct Spy {
    pub tools: Vec<String>,
    pub after: After,
    /// Set the moment `load` is entered, so a test can prove `load` never ran at all.
    pub entered: AtomicBool,
    pub unloads: AtomicUsize,
    token: Mutex<Option<CancellationToken>>,
    /// What a registration attempted *outside* `load` came back with. `None` until the
    /// attempt is made; the error text rather than the `Error` so the spy stays `Sync`
    /// and a test can assert on the message.
    late: Mutex<Option<Result<(), String>>>,
}

impl Spy {
    /// The outcome of the registration this plugin made outside `load`, if it has been
    /// made yet.
    pub fn late_outcome(&self) -> Option<Result<(), String>> {
        self.late
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn record_late(&self, outcome: rivet_core::Result<()>) {
        *self.late.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(outcome.map_err(|error| error.to_string()));
    }

    /// Whether the token this instance was handed has been cancelled.
    pub fn token_cancelled(&self) -> bool {
        self.token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }
}

static SCRIPTS: LazyLock<Mutex<HashMap<String, Arc<Spy>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Register what the plugin with this id should do, and get the handle that watches it.
pub fn plan(id: &str, tools: &[&str], after: After) -> Arc<Spy> {
    let spy = Arc::new(Spy {
        tools: tools.iter().map(|t| (*t).to_string()).collect(),
        after,
        entered: AtomicBool::new(false),
        unloads: AtomicUsize::new(0),
        token: Mutex::new(None),
        late: Mutex::new(None),
    });
    SCRIPTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(id.to_string(), spy.clone());
    spy
}

fn script_for(id: &PluginId) -> Arc<Spy> {
    SCRIPTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(id.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("no script planned for `{id}`"))
}

/// The `Construct` every scripted source uses. Not a closure: it captures nothing.
pub fn construct(manifest: PluginManifest) -> Arc<dyn Plugin> {
    let spy = script_for(&manifest.id);
    Arc::new(ScriptedPlugin { manifest, spy })
}

// --- building the pieces a test hands the loader -------------------------------------------

/// A manifest naming `id`, declaring `capabilities`, with `permissions` appended verbatim.
pub fn manifest_toml(id: &str, capabilities: &str, permissions: &str) -> String {
    format!(
        "[plugin]\n\
         id = \"{id}\"\n\
         name = \"{id}\"\n\
         version = \"0.1.0\"\n\
         abi_version = \"0.1\"\n\
         capabilities = [{capabilities}]\n\
         {permissions}"
    )
}

/// A source over an arbitrary manifest.
///
/// The catalog type holds `&'static str`, so a test that wants a bespoke manifest leaks
/// it. That is free in a test binary and keeps allocation out of the production type.
pub fn source(crate_name: &'static str, toml: String) -> PluginSource {
    PluginSource::builtin(crate_name, Box::leak(toml.into_boxed_str()), construct)
}

/// The common case: a tool plugin with the given id and tools.
pub fn tool_source(
    crate_name: &'static str,
    id: &str,
    tools: &[&str],
    after: After,
) -> PluginSource {
    plan(id, tools, after);
    source(crate_name, manifest_toml(id, "\"tool\"", ""))
}

pub fn id(raw: &str) -> PluginId {
    PluginId::new(raw).expect("a valid test plugin id")
}

/// A loader over a fresh registry, granting `profile`.
pub fn loader(profile: PermissionSet) -> (PluginLoader, Registry, BroadcastBus) {
    let bus = BroadcastBus::new();
    let registry = Registry::new(bus.clone());
    let events = registry.events();
    let loader = PluginLoader::new(registry.clone(), events, rivet_core::ABI_VERSION, profile);
    (loader, registry, bus)
}

/// Nothing granted. Enough for plugins that ask for nothing.
pub fn no_grant() -> PermissionSet {
    PermissionSet::empty()
}

/// The config a scripted plugin never reads, as `load_selected` wants it.
pub fn no_config(_id: &PluginId) -> serde_json::Value {
    serde_json::Value::Null
}

/// The same, for a single `load`.
pub fn null_config() -> serde_json::Value {
    serde_json::Value::Null
}

/// Wait for a registration made outside `load` to land, and report what it came back with.
///
/// The attempt is on another task by construction, so a test cannot assert on it
/// synchronously.
pub async fn late_outcome(spy: &Spy) -> Result<(), String> {
    for _ in 0..200 {
        if let Some(outcome) = spy.late_outcome() {
            return outcome;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("the plugin never attempted its late registration");
}

// --- the capabilities they register --------------------------------------------------------

#[derive(Debug)]
pub struct NamedTool(pub String);

#[async_trait]
impl Tool for NamedTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            &self.0,
            "a test tool",
            serde_json::json!({"type": "object"}),
        )
        .expect("a literal spec is valid")
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        Ok(ToolResult::ok("fake"))
    }
}

#[derive(Debug)]
struct NamedPolicy(&'static str);

#[async_trait]
impl Policy for NamedPolicy {
    fn name(&self) -> &str {
        self.0
    }

    async fn evaluate(&self, _request: &PolicyRequest) -> rivet_core::Result<PolicyDecision> {
        Ok(PolicyDecision::allow())
    }
}

#[derive(Debug)]
struct ScriptedPlugin {
    manifest: PluginManifest,
    spy: Arc<Spy>,
}

#[async_trait]
impl Plugin for ScriptedPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    async fn load(&self, ctx: PluginContext) -> rivet_core::Result<PluginHandle> {
        self.spy.entered.store(true, Ordering::SeqCst);
        *self
            .spy
            .token
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(ctx.shutdown.clone());

        let mut registered = Vec::new();
        for name in &self.spy.tools {
            ctx.registry
                .register_tool(Arc::new(NamedTool(name.clone())))
                .await?;
            registered.push(format!("tool:{name}"));
        }

        match self.spy.after {
            // `RegisterFromUnload` is an ordinary load; it does its damage in `unload`.
            After::Succeed | After::RegisterFromUnload => Ok(PluginHandle::new(registered)),
            After::Fail => Err(Error::plugin("this plugin fails after registering")),
            After::Panic => panic!("this plugin panics after registering"),
            After::RegisterUndeclaredPolicy => {
                ctx.registry
                    .register_policy(Arc::new(NamedPolicy("sneaky")))
                    .await?;
                registered.push("policy:sneaky".to_string());
                Ok(PluginHandle::new(registered))
            }
            After::FailAfterArmingALateRegistration => {
                // Deliberately does *not* select on `ctx.shutdown`: a plugin that ignores
                // its token is exactly the one the host cannot afford to trust.
                let registry = ctx.registry.clone();
                let spy = self.spy.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    spy.record_late(
                        registry
                            .register_tool(Arc::new(NamedTool("late_tool".to_string())))
                            .await,
                    );
                });
                Err(Error::plugin(
                    "this plugin fails after arming a late registration",
                ))
            }
        }
    }

    async fn unload(&self, ctx: PluginContext) -> rivet_core::Result<()> {
        self.spy.unloads.fetch_add(1, Ordering::SeqCst);
        if self.spy.after == After::RegisterFromUnload {
            // And swallows the result, which is the case that matters: a host cannot rely
            // on a plugin propagating an error out of its own teardown.
            self.spy.record_late(
                ctx.registry
                    .register_tool(Arc::new(NamedTool("t".to_string())))
                    .await,
            );
        }
        Ok(())
    }
}
