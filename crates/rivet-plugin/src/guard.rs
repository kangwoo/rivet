//! The registry a plugin actually sees during `load`.
//!
//! [`rivet_core::plugin::PluginManifest::capabilities`] already promises that "registering into a slot not
//! declared here is a contract violation and the loader rejects it". Nothing enforced it,
//! so a manifest saying `capabilities = ["tool"]` could quietly register a policy.
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

/// A [`ScopedRegistry`] that refuses undeclared slots and remembers what was registered.
#[derive(Debug)]
pub struct GuardedRegistry {
    inner: ScopedRegistry,
    plugin_id: PluginId,
    declared: Vec<CapabilityKind>,
    observed: Mutex<Vec<String>>,
}

impl GuardedRegistry {
    #[must_use]
    pub fn new(inner: ScopedRegistry, plugin_id: PluginId, declared: Vec<CapabilityKind>) -> Self {
        Self {
            inner,
            plugin_id,
            declared,
            observed: Mutex::new(Vec::new()),
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

/// Every method is: check the slot, delegate, record the name. The labels match the ones
/// [`rivet_runtime::Registry::unregister_all`] returns, so what a plugin registered and
/// what a rollback removed are the same strings.
#[async_trait]
impl PluginRegistry for GuardedRegistry {
    async fn register_model(&self, model: Arc<dyn Model>) -> rivet_core::Result<()> {
        self.require(CapabilityKind::Model)?;
        let name = model.id().as_str().to_string();
        self.inner.register_model(model).await?;
        self.record("model", &name);
        Ok(())
    }

    async fn register_tool(&self, tool: Arc<dyn Tool>) -> rivet_core::Result<()> {
        self.require(CapabilityKind::Tool)?;
        let name = tool.spec().name;
        self.inner.register_tool(tool).await?;
        self.record("tool", &name);
        Ok(())
    }

    async fn register_context_provider(
        &self,
        provider: Arc<dyn ContextProvider>,
    ) -> rivet_core::Result<()> {
        self.require(CapabilityKind::ContextProvider)?;
        let name = provider.name().to_string();
        self.inner.register_context_provider(provider).await?;
        self.record("context", &name);
        Ok(())
    }

    async fn register_policy(&self, policy: Arc<dyn Policy>) -> rivet_core::Result<()> {
        self.require(CapabilityKind::Policy)?;
        let name = policy.name().to_string();
        self.inner.register_policy(policy).await?;
        self.record("policy", &name);
        Ok(())
    }

    async fn register_sandbox(&self, sandbox: Arc<dyn Sandbox>) -> rivet_core::Result<()> {
        self.require(CapabilityKind::Sandbox)?;
        let name = sandbox.name().to_string();
        self.inner.register_sandbox(sandbox).await?;
        self.record("sandbox", &name);
        Ok(())
    }

    async fn register_memory(&self, memory: Arc<dyn Memory>) -> rivet_core::Result<()> {
        self.require(CapabilityKind::Memory)?;
        let name = memory.name().to_string();
        self.inner.register_memory(memory).await?;
        self.record("memory", &name);
        Ok(())
    }

    async fn register_workflow(&self, workflow: Arc<dyn Workflow>) -> rivet_core::Result<()> {
        self.require(CapabilityKind::Workflow)?;
        let name = workflow.name().to_string();
        self.inner.register_workflow(workflow).await?;
        self.record("workflow", &name);
        Ok(())
    }

    async fn register_scheduler(&self, scheduler: Arc<dyn Scheduler>) -> rivet_core::Result<()> {
        self.require(CapabilityKind::Scheduler)?;
        let name = scheduler.name().to_string();
        self.inner.register_scheduler(scheduler).await?;
        self.record("scheduler", &name);
        Ok(())
    }

    async fn register_subscriber(
        &self,
        subscriber: Arc<dyn EventSubscriber>,
    ) -> rivet_core::Result<()> {
        self.require(CapabilityKind::EventSubscriber)?;
        let name = subscriber.name().to_string();
        self.inner.register_subscriber(subscriber).await?;
        self.record("subscriber", &name);
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
        self.require(CapabilityKind::Policy)?;
        let name = interceptor.name().to_string();
        self.inner.register_interceptor(interceptor).await?;
        self.record("interceptor", &name);
        Ok(())
    }

    async fn register_session_store(&self, store: Arc<dyn SessionStore>) -> rivet_core::Result<()> {
        self.require(CapabilityKind::SessionStore)?;
        // The registry keys session stores by plugin id, so that is the name to record.
        let name = self.plugin_id.as_str().to_string();
        self.inner.register_session_store(store).await?;
        self.record("session_store", &name);
        Ok(())
    }

    async fn register_evaluator(&self, evaluator: Arc<dyn Evaluator>) -> rivet_core::Result<()> {
        self.require(CapabilityKind::Evaluator)?;
        let name = evaluator.name().to_string();
        self.inner.register_evaluator(evaluator).await?;
        self.record("evaluator", &name);
        Ok(())
    }
}
