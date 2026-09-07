//! The registry a plugin actually sees during `load`.
//!
//! [`rivet_core::plugin::PluginManifest::capabilities`] already promises that "registering into a slot not
//! declared here is a contract violation and the loader rejects it". Nothing enforced it,
//! so a manifest saying `capabilities = ["tool"]` could quietly register a policy.
//!
//! The guard also *closes*. Registration is an act of `load`; once the loader has sealed
//! the guard (after `load` returns, and again before `unload` runs) every `register_*`
//! fails loudly, naming the plugin. Without that, the rollback on a failed load is a
//! point-in-time sweep of a handle the plugin still holds — see `GuardedRegistry::seal`,
//! which is crate-private: the loader owns the window's lifecycle, not the embedder.
//!
//! The guard also *records* what was registered. That observed list — not the plugin's
//! self-reported [`PluginHandle`](rivet_core::plugin::PluginHandle) — is what
//! `rivet plugin list` and `rivet doctor` print, because a handle is a claim and the
//! registry calls are the fact.
//!
//! It wraps [`ScopedRegistry`] rather than living inside it: the registry does not see the
//! manifest, and adding a declared-kinds parameter to `Registry::scoped` would change a
//! Phase 0 contract for every embedder.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rivet_core::capability::CapabilityKind;
use rivet_core::context::ContextProvider;
use rivet_core::error::Error;
use rivet_core::event::EventSubscriber;
use rivet_core::id::PluginId;
use rivet_core::job::{Scheduler, Workflow};
use rivet_core::memory::{Evaluator, Memory};
use rivet_core::model::Model;
use rivet_core::plugin::{Interceptor, PluginRegistry};
use rivet_core::policy::Policy;
use rivet_core::sandbox::Sandbox;
use rivet_core::session::SessionStore;
use rivet_core::tool::Tool;
use rivet_runtime::registry::ScopedRegistry;
use tokio::sync::{RwLock, RwLockReadGuard};

/// A [`ScopedRegistry`] that refuses undeclared slots and remembers what was registered.
#[derive(Debug)]
pub struct GuardedRegistry {
    inner: ScopedRegistry,
    plugin_id: PluginId,
    declared: Vec<CapabilityKind>,
    observed: Mutex<Vec<String>>,
    /// `true` once the loader has closed the registration window.
    ///
    /// A lock rather than an `AtomicBool` because `seal` has to *wait out*
    /// the registrations already in flight: every `register_*` holds the read side across
    /// its whole check-delegate-record sequence, so once `seal` has taken the write side
    /// the observed list can no longer grow. With a flag, a registration that had already
    /// passed the check could still land after the loader read `observed()` — and a
    /// capability missing from `record.registered` is the bug this exists to close.
    sealed: RwLock<bool>,
}

impl GuardedRegistry {
    #[must_use]
    pub fn new(inner: ScopedRegistry, plugin_id: PluginId, declared: Vec<CapabilityKind>) -> Self {
        Self {
            inner,
            plugin_id,
            declared,
            observed: Mutex::new(Vec::new()),
            sealed: RwLock::new(false),
        }
    }

    /// What this plugin registered, in registration order.
    #[must_use]
    pub fn observed(&self) -> Vec<String> {
        self.observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Close the registration window: every later `register_*` fails.
    ///
    /// Crate-private on purpose. `GuardedRegistry` is re-exported so an embedder can build
    /// its own, but the window belongs to the lifecycle, not to the holder of the guard: a
    /// seal before `load` would make every honest registration fail with a message saying
    /// the load window closed — true, and useless. Only [`PluginLoader`](crate::PluginLoader)
    /// knows when `load` began and ended, so only it can call this.
    ///
    /// Registration is an act of `Plugin::load` and of nothing else. The guard outlives
    /// that call — [`PluginContext`](rivet_core::plugin::PluginContext) is `Clone` and its
    /// `registry` is an `Arc` — so without this the loader's rollback is a point-in-time
    /// sweep rather than a seal: a task the plugin spawned could register into an instance
    /// that has already failed, owned by an instance id no `unload` will ever mention, and
    /// invisible to `rivet doctor` and `rivet plugin list` (both read `record.registered`).
    ///
    /// The loader calls this after `Plugin::load` returns, on both the success and the
    /// failure path, and again before `Plugin::unload` runs — where a registration would
    /// otherwise survive the `unregister_all` that precedes it and hold the name against
    /// the next load. Idempotent.
    pub(crate) async fn seal(&self) {
        *self.sealed.write().await = true;
    }

    /// Hold the registration window open for one `register_*`, if the slot is declared.
    ///
    /// The caller keeps the returned guard until it has recorded the registration. That is
    /// what makes `seal` a fence and not a flag.
    async fn open(&self, kind: CapabilityKind) -> rivet_core::Result<RwLockReadGuard<'_, bool>> {
        let window = self.sealed.read().await;
        if *window {
            // Loud as well as fallible: the caller is a task the loader cannot see, and it
            // is free to drop the `Err` on the floor. Then this line is the only trace.
            tracing::warn!(
                plugin = %self.plugin_id,
                capability = kind_name(kind),
                "a plugin tried to register after its load window closed"
            );
            return Err(Error::plugin(format!(
                "`{}` tried to register a `{}` capability after its load window closed; a \
                 plugin registers from inside `Plugin::load` and nowhere else",
                self.plugin_id,
                kind_name(kind)
            )));
        }
        self.require(kind)?;
        Ok(window)
    }

    /// Refuse a slot the manifest did not declare.
    fn require(&self, kind: CapabilityKind) -> rivet_core::Result<()> {
        if self.declared.contains(&kind) {
            return Ok(());
        }
        let declared = self
            .declared
            .iter()
            .map(|k| kind_name(*k))
            .collect::<Vec<_>>()
            .join(", ");
        Err(Error::plugin(format!(
            "`{}` tried to register a `{}` capability, but its manifest declares only [{declared}]",
            self.plugin_id,
            kind_name(kind)
        )))
    }

    fn record(&self, label: &str, name: &str) {
        self.observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{label}:{name}"));
    }
}

/// The `snake_case` name of a slot, as it is spelled in a manifest's `capabilities`.
///
/// Written out rather than derived from serde so that adding a variant to
/// [`CapabilityKind`] fails to compile here instead of producing a stringly-typed
/// mismatch at runtime.
const fn kind_name(kind: CapabilityKind) -> &'static str {
    match kind {
        CapabilityKind::Model => "model",
        CapabilityKind::Tool => "tool",
        CapabilityKind::ContextProvider => "context_provider",
        CapabilityKind::Policy => "policy",
        CapabilityKind::Sandbox => "sandbox",
        CapabilityKind::SessionStore => "session_store",
        CapabilityKind::Memory => "memory",
        CapabilityKind::Workflow => "workflow",
        CapabilityKind::Scheduler => "scheduler",
        CapabilityKind::Evaluator => "evaluator",
        CapabilityKind::EventSubscriber => "event_subscriber",
        CapabilityKind::Command => "command",
    }
}

/// Every method is: open the window, check the slot, delegate, record the name. The window
/// stays open across that whole sequence, so a `seal` racing a registration either loses —
/// the registration lands and is recorded — or wins, and the registration never starts.
/// Never half of each.
///
/// The labels match the ones [`rivet_runtime::Registry::unregister_all`] returns, so what a
/// plugin registered and what a rollback removed are the same strings.
#[async_trait]
impl PluginRegistry for GuardedRegistry {
    async fn register_model(&self, model: Arc<dyn Model>) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::Model).await?;
        let name = model.id().as_str().to_string();
        self.inner.register_model(model).await?;
        self.record("model", &name);
        drop(window);
        Ok(())
    }

    async fn register_tool(&self, tool: Arc<dyn Tool>) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::Tool).await?;
        let name = tool.spec().name;
        self.inner.register_tool(tool).await?;
        self.record("tool", &name);
        drop(window);
        Ok(())
    }

    async fn register_context_provider(
        &self,
        provider: Arc<dyn ContextProvider>,
    ) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::ContextProvider).await?;
        let name = provider.name().to_string();
        self.inner.register_context_provider(provider).await?;
        self.record("context", &name);
        drop(window);
        Ok(())
    }

    async fn register_policy(&self, policy: Arc<dyn Policy>) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::Policy).await?;
        let name = policy.name().to_string();
        self.inner.register_policy(policy).await?;
        self.record("policy", &name);
        drop(window);
        Ok(())
    }

    async fn register_sandbox(&self, sandbox: Arc<dyn Sandbox>) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::Sandbox).await?;
        let name = sandbox.name().to_string();
        self.inner.register_sandbox(sandbox).await?;
        self.record("sandbox", &name);
        drop(window);
        Ok(())
    }

    async fn register_memory(&self, memory: Arc<dyn Memory>) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::Memory).await?;
        let name = memory.name().to_string();
        self.inner.register_memory(memory).await?;
        self.record("memory", &name);
        drop(window);
        Ok(())
    }

    async fn register_workflow(&self, workflow: Arc<dyn Workflow>) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::Workflow).await?;
        let name = workflow.name().to_string();
        self.inner.register_workflow(workflow).await?;
        self.record("workflow", &name);
        drop(window);
        Ok(())
    }

    async fn register_scheduler(&self, scheduler: Arc<dyn Scheduler>) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::Scheduler).await?;
        let name = scheduler.name().to_string();
        self.inner.register_scheduler(scheduler).await?;
        self.record("scheduler", &name);
        drop(window);
        Ok(())
    }

    async fn register_subscriber(
        &self,
        subscriber: Arc<dyn EventSubscriber>,
    ) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::EventSubscriber).await?;
        let name = subscriber.name().to_string();
        self.inner.register_subscriber(subscriber).await?;
        self.record("subscriber", &name);
        drop(window);
        Ok(())
    }

    /// `CapabilityKind` has no `Interceptor` variant, so an interceptor is declared as
    /// `policy`: both answer "may this tool call proceed", and widening the closed
    /// vocabulary is a `rivet-core` change Phase 2's scope boundary rules out. Worth
    /// revisiting in Phase 4, when interceptors actually run.
    async fn register_interceptor(
        &self,
        interceptor: Arc<dyn Interceptor>,
    ) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::Policy).await?;
        let name = interceptor.name().to_string();
        self.inner.register_interceptor(interceptor).await?;
        self.record("interceptor", &name);
        drop(window);
        Ok(())
    }

    async fn register_session_store(&self, store: Arc<dyn SessionStore>) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::SessionStore).await?;
        // The registry keys session stores by plugin id, so that is the name to record.
        let name = self.plugin_id.as_str().to_string();
        self.inner.register_session_store(store).await?;
        self.record("session_store", &name);
        drop(window);
        Ok(())
    }

    async fn register_evaluator(&self, evaluator: Arc<dyn Evaluator>) -> rivet_core::Result<()> {
        let window = self.open(CapabilityKind::Evaluator).await?;
        let name = evaluator.name().to_string();
        self.inner.register_evaluator(evaluator).await?;
        self.record("evaluator", &name);
        drop(window);
        Ok(())
    }
}
