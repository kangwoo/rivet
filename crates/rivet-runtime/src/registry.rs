//! Capability registries with **ownership tracking**.
//!
//! Every registration records the plugin instance that made it. That single field is what
//! makes unload, hot reload, and load-failure rollback possible: "remove everything
//! `rivet.tool-git` registered" is a query, not a bookkeeping exercise spread across
//! plugin authors.
//!
//! Name collisions are rejected rather than silently overwritten. Two plugins both
//! registering `shell` is a configuration bug, and the failure should name both.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::context::ContextProvider;
use rivet_core::error::{Error, Result};
use rivet_core::event::{EventBus, EventSubscriber};
use rivet_core::id::{PluginId, PluginInstanceId};
use rivet_core::job::{Scheduler, Workflow};
use rivet_core::memory::Evaluator;
use rivet_core::memory::Memory;
use rivet_core::model::{Model, ModelId};
use rivet_core::plugin::{Interceptor, PluginRegistry};
use rivet_core::policy::Policy;
use rivet_core::sandbox::Sandbox;
use rivet_core::session::SessionStore;
use rivet_core::tool::Tool;
use tokio::sync::RwLock;

use crate::bus::BroadcastBus;
use tokio::task::JoinHandle;

/// Who registered an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub plugin_id: PluginId,
    pub instance_id: PluginInstanceId,
}

/// One registered capability.
#[derive(Clone)]
struct Entry<T: ?Sized> {
    owner: Owner,
    value: Arc<T>,
}

/// A name-keyed table of one capability kind.
struct Table<T: ?Sized> {
    entries: HashMap<String, Entry<T>>,
    kind: &'static str,
}

impl<T: ?Sized> Table<T> {
    fn new(kind: &'static str) -> Self {
        Self {
            entries: HashMap::new(),
            kind,
        }
    }

    fn insert(&mut self, name: String, owner: Owner, value: Arc<T>) -> Result<()> {
        if let Some(existing) = self.entries.get(&name) {
            return Err(Error::plugin(format!(
                "{} `{name}` is already registered by `{}`; \
                 `{}` cannot register it again",
                self.kind, existing.owner.plugin_id, owner.plugin_id
            )));
        }
        self.entries.insert(name, Entry { owner, value });
        Ok(())
    }

    fn get(&self, name: &str) -> Option<Arc<T>> {
        self.entries.get(name).map(|e| e.value.clone())
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.entries.keys().cloned().collect();
        names.sort();
        names
    }

    /// Remove everything owned by `instance`, returning the removed names.
    fn remove_owned_by(&mut self, instance: PluginInstanceId) -> Vec<String> {
        let removed: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.owner.instance_id == instance)
            .map(|(name, _)| name.clone())
            .collect();
        for name in &removed {
            self.entries.remove(name);
        }
        removed
    }
}

/// All capability tables.
#[derive(Default)]
struct Tables {
    models: Option<Table<dyn Model>>,
    tools: Option<Table<dyn Tool>>,
    contexts: Option<Table<dyn ContextProvider>>,
    policies: Option<Table<dyn Policy>>,
    sandboxes: Option<Table<dyn Sandbox>>,
    memories: Option<Table<dyn Memory>>,
    workflows: Option<Table<dyn Workflow>>,
    schedulers: Option<Table<dyn Scheduler>>,
    subscribers: Option<Table<dyn EventSubscriber>>,
    interceptors: Option<Table<dyn Interceptor>>,
    session_stores: Option<Table<dyn SessionStore>>,
    evaluators: Option<Table<dyn Evaluator>>,
    /// Background jobs pumping events into subscribers, keyed by owner. Held here so
    /// `unregister_all` can abort them: an unloaded plugin whose task keeps running would
    /// go on observing every event in the process.
    subscriber_tasks: Vec<(PluginInstanceId, JoinHandle<()>)>,
}

macro_rules! table {
    ($self:ident, $field:ident, $kind:literal) => {
        $self.$field.get_or_insert_with(|| Table::new($kind))
    };
}

/// The runtime's capability registry.
///
/// Cheap to clone; all clones share one set of tables.
#[derive(Clone)]
pub struct Registry {
    tables: Arc<RwLock<Tables>>,
    /// The concrete bus, not `Arc<dyn EventBus>`, because registering a subscriber must
    /// also *start delivering to it*. Splitting those into two calls made it possible to
    /// register a subscriber that silently received nothing.
    bus: BroadcastBus,
}

impl Registry {
    #[must_use]
    pub fn new(bus: BroadcastBus) -> Self {
        Self {
            tables: Arc::new(RwLock::new(Tables::default())),
            bus,
        }
    }

    /// A registry scoped to one plugin instance. This is what gets handed to
    /// [`rivet_core::plugin::Plugin::load`], so a plugin cannot register on another's
    /// behalf — the owner is bound here, not supplied by the caller.
    #[must_use]
    pub fn scoped(&self, owner: Owner) -> ScopedRegistry {
        ScopedRegistry {
            registry: self.clone(),
            owner,
        }
    }

    pub async fn model(&self, id: &ModelId) -> Option<Arc<dyn Model>> {
        self.tables
            .read()
            .await
            .models
            .as_ref()
            .and_then(|t| t.get(id.as_str()))
    }

    pub async fn tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tables
            .read()
            .await
            .tools
            .as_ref()
            .and_then(|t| t.get(name))
    }

    pub async fn tool_names(&self) -> Vec<String> {
        self.tables
            .read()
            .await
            .tools
            .as_ref()
            .map(Table::names)
            .unwrap_or_default()
    }

    pub async fn policies(&self) -> Vec<Arc<dyn Policy>> {
        self.tables
            .read()
            .await
            .policies
            .as_ref()
            .map(|t| {
                let mut names = t.names();
                names.sort();
                names.iter().filter_map(|n| t.get(n)).collect()
            })
            .unwrap_or_default()
    }

    pub async fn sandbox(&self, name: &str) -> Option<Arc<dyn Sandbox>> {
        self.tables
            .read()
            .await
            .sandboxes
            .as_ref()
            .and_then(|t| t.get(name))
    }

    pub async fn context_providers(&self) -> Vec<Arc<dyn ContextProvider>> {
        self.tables
            .read()
            .await
            .contexts
            .as_ref()
            .map(|t| t.names().iter().filter_map(|n| t.get(n)).collect())
            .unwrap_or_default()
    }

    pub async fn workflow(&self, name: &str) -> Option<Arc<dyn Workflow>> {
        self.tables
            .read()
            .await
            .workflows
            .as_ref()
            .and_then(|t| t.get(name))
    }

    pub async fn subscribers(&self) -> Vec<Arc<dyn EventSubscriber>> {
        self.tables
            .read()
            .await
            .subscribers
            .as_ref()
            .map(|t| t.names().iter().filter_map(|n| t.get(n)).collect())
            .unwrap_or_default()
    }

    pub async fn memory(&self, name: &str) -> Option<Arc<dyn Memory>> {
        self.tables
            .read()
            .await
            .memories
            .as_ref()
            .and_then(|t| t.get(name))
    }

    pub async fn scheduler(&self, name: &str) -> Option<Arc<dyn Scheduler>> {
        self.tables
            .read()
            .await
            .schedulers
            .as_ref()
            .and_then(|t| t.get(name))
    }

    pub async fn session_store(&self, name: &str) -> Option<Arc<dyn SessionStore>> {
        self.tables
            .read()
            .await
            .session_stores
            .as_ref()
            .and_then(|t| t.get(name))
    }

    pub async fn evaluators(&self) -> Vec<Arc<dyn Evaluator>> {
        self.tables
            .read()
            .await
            .evaluators
            .as_ref()
            .map(|t| t.names().iter().filter_map(|n| t.get(n)).collect())
            .unwrap_or_default()
    }

    /// Interceptors in evaluation order: `priority` ascending, then name.
    ///
    /// Sorted rather than returned in registration order so that which reason a user sees
    /// first is a property of configuration, not of plugin load timing.
    pub async fn interceptors(&self) -> Vec<Arc<dyn Interceptor>> {
        self.tables
            .read()
            .await
            .interceptors
            .as_ref()
            .map(|t| {
                let mut found: Vec<Arc<dyn Interceptor>> =
                    t.names().iter().filter_map(|n| t.get(n)).collect();
                found.sort_by(|a, b| a.priority().cmp(&b.priority()).then(a.name().cmp(b.name())));
                found
            })
            .unwrap_or_default()
    }

    /// Remove every capability registered by `instance`.
    ///
    /// This is both the unload path and the rollback path for a plugin whose `load`
    /// failed halfway through.
    pub async fn unregister_all(&self, instance: PluginInstanceId) -> Vec<String> {
        let mut tables = self.tables.write().await;
        let mut removed = Vec::new();
        macro_rules! sweep {
            ($field:ident, $label:literal) => {
                if let Some(t) = tables.$field.as_mut() {
                    removed.extend(
                        t.remove_owned_by(instance)
                            .into_iter()
                            .map(|n| format!(concat!($label, ":{}"), n)),
                    );
                }
            };
        }
        sweep!(models, "model");
        sweep!(tools, "tool");
        sweep!(contexts, "context");
        sweep!(policies, "policy");
        sweep!(sandboxes, "sandbox");
        sweep!(memories, "memory");
        sweep!(workflows, "workflow");
        sweep!(schedulers, "scheduler");
        sweep!(subscribers, "subscriber");
        sweep!(interceptors, "interceptor");
        sweep!(session_stores, "session_store");
        sweep!(evaluators, "evaluator");

        // Stop this plugin's event pumps. Without this an unloaded plugin keeps observing
        // every event in the process for the lifetime of the runtime.
        let mut still_running = Vec::new();
        for (owner, handle) in std::mem::take(&mut tables.subscriber_tasks) {
            if owner == instance {
                handle.abort();
            } else {
                still_running.push((owner, handle));
            }
        }
        tables.subscriber_tasks = still_running;

        removed.sort();
        removed
    }

    /// Start the event pump for a subscriber and remember the task under `owner`.
    ///
    /// Called automatically by `register_subscriber`; exposed for hosts that drive
    /// subscribers outside the plugin lifecycle.
    pub async fn attach_subscriber(&self, owner: &Owner, subscriber: Arc<dyn EventSubscriber>) {
        let handle = self.bus.attach(subscriber);
        self.tables
            .write()
            .await
            .subscriber_tasks
            .push((owner.instance_id, handle));
    }

    /// The event bus, for plugins that need to publish.
    #[must_use]
    pub fn events(&self) -> Arc<dyn EventBus> {
        Arc::new(self.bus.clone())
    }

    /// The concrete bus, for clients that subscribe (the TUI, the JSONL writer).
    #[must_use]
    pub fn bus(&self) -> &BroadcastBus {
        &self.bus
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Registry")
    }
}

/// A [`Registry`] view bound to one plugin instance.
#[derive(Clone)]
pub struct ScopedRegistry {
    registry: Registry,
    owner: Owner,
}

impl fmt::Debug for ScopedRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedRegistry")
            .field("owner", &self.owner.plugin_id)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl PluginRegistry for ScopedRegistry {
    async fn register_model(&self, model: Arc<dyn Model>) -> Result<()> {
        let name = model.id().as_str().to_string();
        let mut tables = self.registry.tables.write().await;
        table!(tables, models, "model").insert(name, self.owner.clone(), model)
    }

    async fn register_tool(&self, tool: Arc<dyn Tool>) -> Result<()> {
        let name = tool.spec().name;
        let mut tables = self.registry.tables.write().await;
        table!(tables, tools, "tool").insert(name, self.owner.clone(), tool)
    }

    async fn register_context_provider(&self, provider: Arc<dyn ContextProvider>) -> Result<()> {
        let name = provider.name().to_string();
        let mut tables = self.registry.tables.write().await;
        table!(tables, contexts, "context provider").insert(name, self.owner.clone(), provider)
    }

    async fn register_policy(&self, policy: Arc<dyn Policy>) -> Result<()> {
        let name = policy.name().to_string();
        let mut tables = self.registry.tables.write().await;
        table!(tables, policies, "policy").insert(name, self.owner.clone(), policy)
    }

    async fn register_sandbox(&self, sandbox: Arc<dyn Sandbox>) -> Result<()> {
        let name = sandbox.name().to_string();
        let mut tables = self.registry.tables.write().await;
        table!(tables, sandboxes, "sandbox").insert(name, self.owner.clone(), sandbox)
    }

    async fn register_memory(&self, memory: Arc<dyn Memory>) -> Result<()> {
        let name = memory.name().to_string();
        let mut tables = self.registry.tables.write().await;
        table!(tables, memories, "memory").insert(name, self.owner.clone(), memory)
    }

    async fn register_workflow(&self, workflow: Arc<dyn Workflow>) -> Result<()> {
        let name = workflow.name().to_string();
        let mut tables = self.registry.tables.write().await;
        table!(tables, workflows, "workflow").insert(name, self.owner.clone(), workflow)
    }

    async fn register_scheduler(&self, scheduler: Arc<dyn Scheduler>) -> Result<()> {
        let name = scheduler.name().to_string();
        let mut tables = self.registry.tables.write().await;
        table!(tables, schedulers, "scheduler").insert(name, self.owner.clone(), scheduler)
    }

    async fn register_subscriber(&self, subscriber: Arc<dyn EventSubscriber>) -> Result<()> {
        let name = subscriber.name().to_string();
        {
            let mut tables = self.registry.tables.write().await;
            table!(tables, subscribers, "subscriber").insert(
                name,
                self.owner.clone(),
                subscriber.clone(),
            )?;
        }
        // Register *and* start delivering, in one step. Two separate calls meant a caller
        // could register a subscriber that never received anything, with no error.
        self.registry
            .attach_subscriber(&self.owner, subscriber)
            .await;
        Ok(())
    }

    async fn register_interceptor(&self, interceptor: Arc<dyn Interceptor>) -> Result<()> {
        let name = interceptor.name().to_string();
        let mut tables = self.registry.tables.write().await;
        table!(tables, interceptors, "interceptor").insert(name, self.owner.clone(), interceptor)
    }

    async fn register_session_store(&self, store: Arc<dyn SessionStore>) -> Result<()> {
        let mut tables = self.registry.tables.write().await;
        table!(tables, session_stores, "session store").insert(
            self.owner.plugin_id.as_str().to_string(),
            self.owner.clone(),
            store,
        )
    }

    async fn register_evaluator(&self, evaluator: Arc<dyn Evaluator>) -> Result<()> {
        let name = evaluator.name().to_string();
        let mut tables = self.registry.tables.write().await;
        table!(tables, evaluators, "evaluator").insert(name, self.owner.clone(), evaluator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::BroadcastBus;
    use rivet_core::tool::{ToolContext, ToolResult, ToolSpec};

    #[derive(Debug)]
    struct FakeTool(&'static str);

    #[async_trait]
    impl Tool for FakeTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::new(self.0, "a fake tool", serde_json::json!({"type": "object"})).unwrap()
        }

        async fn execute(
            &self,
            _ctx: ToolContext,
            _input: serde_json::Value,
        ) -> Result<ToolResult> {
            Ok(ToolResult::ok("fake"))
        }
    }

    fn owner(name: &str) -> Owner {
        Owner {
            plugin_id: PluginId::new(name).unwrap(),
            instance_id: PluginInstanceId::new(),
        }
    }

    fn registry() -> Registry {
        Registry::new(BroadcastBus::new())
    }

    #[tokio::test]
    async fn registered_tools_are_resolvable_by_name() {
        let reg = registry();
        let scoped = reg.scoped(owner("rivet.tool-shell"));
        scoped
            .register_tool(Arc::new(FakeTool("shell")))
            .await
            .unwrap();

        assert!(reg.tool("shell").await.is_some());
        assert!(reg.tool("nope").await.is_none());
        assert_eq!(reg.tool_names().await, vec!["shell"]);
    }

    #[tokio::test]
    async fn name_collisions_are_rejected_and_name_both_plugins() {
        let reg = registry();
        reg.scoped(owner("rivet.tool-shell"))
            .register_tool(Arc::new(FakeTool("shell")))
            .await
            .unwrap();

        let err = reg
            .scoped(owner("acme.tool-shell"))
            .register_tool(Arc::new(FakeTool("shell")))
            .await
            .unwrap_err();

        let msg = err.message();
        assert!(msg.contains("rivet.tool-shell"), "{msg}");
        assert!(msg.contains("acme.tool-shell"), "{msg}");
    }

    #[tokio::test]
    async fn unregister_removes_exactly_one_plugins_entries() {
        let reg = registry();
        let a = owner("rivet.tool-shell");
        let b = owner("rivet.tool-git");
        reg.scoped(a.clone())
            .register_tool(Arc::new(FakeTool("shell")))
            .await
            .unwrap();
        reg.scoped(b.clone())
            .register_tool(Arc::new(FakeTool("git_status")))
            .await
            .unwrap();
        reg.scoped(b.clone())
            .register_tool(Arc::new(FakeTool("git_diff")))
            .await
            .unwrap();

        let removed = reg.unregister_all(b.instance_id).await;
        assert_eq!(removed, vec!["tool:git_diff", "tool:git_status"]);
        assert!(reg.tool("shell").await.is_some(), "other plugins untouched");
        assert!(reg.tool("git_status").await.is_none());
    }

    #[tokio::test]
    async fn a_name_is_reusable_after_its_owner_unloads() {
        let reg = registry();
        let first = owner("rivet.tool-shell");
        reg.scoped(first.clone())
            .register_tool(Arc::new(FakeTool("shell")))
            .await
            .unwrap();
        reg.unregister_all(first.instance_id).await;

        // Hot reload: the same name registers cleanly for a new instance.
        reg.scoped(owner("rivet.tool-shell"))
            .register_tool(Arc::new(FakeTool("shell")))
            .await
            .expect("re-registration after unload must succeed");
    }

    #[derive(Debug)]
    struct CountingSubscriber {
        seen: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl rivet_core::event::EventSubscriber for CountingSubscriber {
        fn name(&self) -> &'static str {
            "counter"
        }

        async fn on_event(&self, _envelope: &rivet_core::event::EventEnvelope) {
            self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn an_unloaded_subscriber_stops_receiving_events() {
        use rivet_core::event::{Event, EventBus, EventEnvelope, RuntimeEvent};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let bus = crate::bus::BroadcastBus::new();
        let reg = Registry::new(bus.clone());
        let owner = owner("rivet.telemetry");
        let seen = Arc::new(AtomicUsize::new(0));
        let subscriber = Arc::new(CountingSubscriber { seen: seen.clone() });

        // Registration alone must be enough: no separate attach step to forget.
        reg.scoped(owner.clone())
            .register_subscriber(subscriber)
            .await
            .unwrap();

        let ping = || {
            bus.publish(EventEnvelope::new(Event::Runtime(RuntimeEvent::Started {
                version: "0.1.0".into(),
            })));
        };

        ping();
        for _ in 0..100 {
            if seen.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let before = seen.load(Ordering::SeqCst);
        assert!(
            before > 0,
            "a registered subscriber must actually receive events"
        );

        reg.unregister_all(owner.instance_id).await;
        // Give the abort time to land, then verify nothing more arrives.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        for _ in 0..5 {
            ping();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert_eq!(
            seen.load(Ordering::SeqCst),
            before,
            "an unloaded plugin must stop observing the process"
        );
    }

    #[tokio::test]
    async fn every_capability_slot_is_readable() {
        // A slot you can register into but never read from is a write-only hole; Phase 3
        // discovered this the hard way with subscribers.
        let reg = registry();
        assert!(reg.subscribers().await.is_empty());
        assert!(reg.evaluators().await.is_empty());
        assert!(reg.memory("none").await.is_none());
        assert!(reg.scheduler("none").await.is_none());
        assert!(reg.session_store("none").await.is_none());
        assert!(reg.interceptors().await.is_empty());
        assert!(reg.context_providers().await.is_empty());
        assert!(reg.policies().await.is_empty());
    }

    #[tokio::test]
    async fn unregistering_an_unknown_instance_is_a_no_op() {
        let reg = registry();
        assert!(reg.unregister_all(PluginInstanceId::new()).await.is_empty());
    }
}
