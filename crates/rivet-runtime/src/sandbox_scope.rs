//! Pipeline step 7: one call's sandbox, owned by the dispatcher.
//!
//! # A missing provider is not a refusal — until a process is asked for
//!
//! Step 7 is on the path of **every** tool call. Making "no such provider" a refusal there
//! would block `read_file` under a profile that grants no `process_spawn` (so no sandbox
//! plugin registers anything), and would block every call in an existing `rivet.toml` that
//! spells out its own `enabled` list. Neither of those configurations ever spawns
//! anything.
//!
//! So the invariant is written the other way round:
//!
//! > **No process starts outside the confinement the decision asked for. And a call that
//! > starts no process is not blocked for want of one.**
//!
//! Step 7 does a registry lookup — cheap — and records the answer on
//! `tool.execute.started.sandboxed`. [`SandboxScope::exec`] is where the absence becomes a
//! failure, naming the provider it could not find. `rivet doctor` runs the same lookup
//! before a run and says so ahead of time.
//!
//! # The dispatcher owns the scope, and `teardown` takes `&self`
//!
//! A tool that ignores cancellation is *abandoned* rather than aborted — aborting mid-write
//! leaves half-written files. If the abandoned task owned the sandbox handle, nothing would
//! be left to call `teardown`, and the child process tree would be orphaned. So the
//! dispatcher holds an `Arc<SandboxScope>` and the tool's host holds another: "no zombies
//! after a cancel" is a property of *ownership*, not of the provider.
//!
//! Which is also why the handle inside is an `Arc` rather than a `Box`. `exec` clones it
//! out of the mutex and releases the lock **before** waiting on the child; a `Box` would
//! have to be borrowed through the guard for the child's whole lifetime, and `teardown`
//! would then block on that same mutex — undoing with a lock exactly what the ownership
//! bought.
//!
//! After teardown the scope is **sealed**: a later `exec` fails instead of preparing a
//! fresh handle. Under `local` a new child would die with the already-cancelled token
//! anyway; under a container provider it would be one more container with nobody left to
//! stop it.

use std::fmt;
use std::sync::Arc;

use rivet_core::error::{Capability, Error, ErrorKind};
use rivet_core::sandbox::{ExecOutput, ExecSpec, Sandbox, SandboxHandle, SandboxRequest};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::registry::Registry;

/// Where the scope's lazily prepared environment has got to.
enum ScopeState {
    Idle,
    Prepared(Arc<dyn SandboxHandle>),
    TornDown,
}

/// One call's confinement.
pub struct SandboxScope {
    name: Option<String>,
    provider: Option<Arc<dyn Sandbox>>,
    request: SandboxRequest,
    state: Mutex<ScopeState>,
}

impl fmt::Debug for SandboxScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SandboxScope")
            .field("name", &self.name)
            .field("registered", &self.provider.is_some())
            .finish_non_exhaustive()
    }
}

impl SandboxScope {
    /// Decide this call's provider name and look it up. **A miss is not an error.**
    ///
    /// `request` is what a later `prepare` will be handed: the workspace, the grant the
    /// chain settled on — the same value the tool receives as
    /// [`rivet_core::tool::ToolContextData::permissions`] — and no `options`. Provider
    /// settings stay with the provider: `sandbox-local` reads its own
    /// `[plugins."rivet.sandbox-local"]` table at load, so there is nothing for the host to
    /// carry.
    pub async fn resolve(registry: &Registry, name: Option<&str>, request: SandboxRequest) -> Self {
        let provider = match name {
            Some(name) => registry.sandbox(name).await,
            None => None,
        };
        Self {
            name: name.map(ToString::to_string),
            provider,
            request,
            state: Mutex::new(ScopeState::Idle),
        }
    }

    /// A scope that will never confine anything, for callers with no registry to ask.
    #[must_use]
    pub fn unconfined(request: SandboxRequest) -> Self {
        Self {
            name: None,
            provider: None,
            request,
            state: Mutex::new(ScopeState::Idle),
        }
    }

    /// The provider name this call resolved to, registered or not.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Whether a registered provider stands behind this call.
    ///
    /// This is the value on `tool.execute.started.sandboxed`, and it means "if this call
    /// spawns a process, it runs under the registered provider" — not "a process ran".
    /// Lazy preparation is why: at publish time nothing has been spawned yet, and eager
    /// preparation is expensive for the providers this shape is aimed at.
    #[must_use]
    pub fn is_sandboxed(&self) -> bool {
        self.provider.is_some()
    }

    /// Run a process, preparing the environment on first use.
    ///
    /// # Errors
    /// - [`ErrorKind::NotFound`] when the decision named a provider nothing registered.
    ///   **This is the only place a missing sandbox stops anything.**
    /// - [`ErrorKind::Cancelled`] once the scope has been torn down.
    /// - Whatever the provider reports.
    pub async fn exec(
        &self,
        spec: ExecSpec,
        cancel: CancellationToken,
    ) -> rivet_core::Result<ExecOutput> {
        let handle = {
            let mut state = self.state.lock().await;
            match &*state {
                ScopeState::TornDown => {
                    return Err(Error::cancelled(
                        "this call's sandbox has already been released; \
                         refusing to start another process under it",
                    ));
                }
                ScopeState::Prepared(handle) => handle.clone(),
                ScopeState::Idle => {
                    let Some(provider) = self.provider.as_ref() else {
                        return Err(Error::new(
                            ErrorKind::NotFound,
                            Capability::Sandbox,
                            match &self.name {
                                Some(name) => format!(
                                    "no sandbox provider named `{name}` is registered, and \
                                     `{}` needs one to run a process",
                                    spec.program
                                ),
                                None => format!(
                                    "no sandbox provider is configured, and `{}` needs one \
                                     to run a process",
                                    spec.program
                                ),
                            },
                        ));
                    };
                    let handle: Arc<dyn SandboxHandle> =
                        Arc::from(provider.prepare(self.request.clone()).await?);
                    *state = ScopeState::Prepared(handle.clone());
                    handle
                }
            }
        };
        // The lock is gone before the child is waited on, so `teardown` can run while a
        // process is still going — which is the whole point of holding this from outside
        // the tool task.
        handle.exec(spec, cancel).await
    }

    /// Release the environment, and seal the scope. Idempotent.
    ///
    /// Takes `&self` because an abandoned tool task still holds an `Arc` to this value, so
    /// the dispatcher cannot take ownership back to release it.
    pub async fn teardown(&self) {
        let prepared = {
            let mut state = self.state.lock().await;
            match std::mem::replace(&mut *state, ScopeState::TornDown) {
                ScopeState::Prepared(handle) => Some(handle),
                ScopeState::Idle | ScopeState::TornDown => None,
            }
        };
        if let Some(handle) = prepared
            && let Err(error) = handle.teardown().await
        {
            tracing::error!(%error, "a sandbox did not release cleanly");
        }
    }
}

impl Drop for SandboxScope {
    /// The contract calls a handle dropped without a teardown "a bug worth logging
    /// loudly". This is the loud part.
    fn drop(&mut self) {
        if matches!(self.state.get_mut(), ScopeState::Prepared(_)) {
            tracing::error!(
                provider = self.name.as_deref().unwrap_or("(none)"),
                "a sandbox scope was dropped without teardown; \
                 its environment may still be running"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rivet_core::capability::PermissionSet;
    use rivet_core::id::SandboxId;
    use rivet_core::workspace::Workspace;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn request() -> SandboxRequest {
        SandboxRequest {
            workspace: Workspace::new(std::path::PathBuf::from("/repo")),
            permissions: PermissionSet::empty(),
            options: serde_json::Map::new(),
        }
    }

    #[derive(Debug, Default)]
    struct CountingHandle {
        torn_down: AtomicUsize,
    }

    #[async_trait]
    impl SandboxHandle for CountingHandle {
        fn id(&self) -> SandboxId {
            SandboxId::new()
        }

        async fn exec(
            &self,
            _spec: ExecSpec,
            _cancel: CancellationToken,
        ) -> rivet_core::Result<ExecOutput> {
            Ok(ExecOutput {
                exit_code: Some(0),
                stdout: "ok".into(),
                stderr: String::new(),
                timed_out: false,
                truncated: false,
                duration_ms: 1,
            })
        }

        async fn teardown(&self) -> rivet_core::Result<()> {
            self.torn_down.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct OneHandle(Arc<CountingHandle>);

    #[async_trait]
    impl Sandbox for OneHandle {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn guarantees(&self) -> rivet_core::sandbox::SandboxGuarantees {
            rivet_core::sandbox::SandboxGuarantees::default()
        }

        async fn prepare(
            &self,
            _request: SandboxRequest,
        ) -> rivet_core::Result<Box<dyn SandboxHandle>> {
            Ok(Box::new(CloneOf(self.0.clone())))
        }
    }

    /// A handle that forwards to the shared counter, so a test can see the teardown.
    #[derive(Debug)]
    struct CloneOf(Arc<CountingHandle>);

    #[async_trait]
    impl SandboxHandle for CloneOf {
        fn id(&self) -> SandboxId {
            self.0.id()
        }

        async fn exec(
            &self,
            spec: ExecSpec,
            cancel: CancellationToken,
        ) -> rivet_core::Result<ExecOutput> {
            self.0.exec(spec, cancel).await
        }

        async fn teardown(&self) -> rivet_core::Result<()> {
            self.0.teardown().await
        }
    }

    async fn registry_with(provider: Arc<CountingHandle>) -> Registry {
        use rivet_core::plugin::PluginRegistry;
        let registry = Registry::new(crate::bus::BroadcastBus::new());
        let owner = crate::registry::Owner {
            plugin_id: rivet_core::id::PluginId::new("rivet.sandbox-test").unwrap(),
            instance_id: rivet_core::id::PluginInstanceId::new(),
        };
        registry
            .scoped(owner)
            .register_sandbox(Arc::new(OneHandle(provider)))
            .await
            .unwrap();
        registry
    }

    #[tokio::test]
    async fn an_unregistered_provider_is_not_an_error_until_a_process_is_asked_for() {
        let registry = Registry::new(crate::bus::BroadcastBus::new());
        let scope = SandboxScope::resolve(&registry, Some("docker"), request()).await;
        assert!(!scope.is_sandboxed(), "nothing registered under that name");

        let error = scope
            .exec(ExecSpec::new("sh", []), CancellationToken::new())
            .await
            .expect_err("a process needs the provider that is missing");
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(error.message().contains("docker"), "{error}");
    }

    #[tokio::test]
    async fn the_scope_is_sealed_after_teardown() {
        let counter = Arc::new(CountingHandle::default());
        let registry = registry_with(counter.clone()).await;
        let scope = SandboxScope::resolve(&registry, Some("counting"), request()).await;

        scope
            .exec(ExecSpec::new("sh", []), CancellationToken::new())
            .await
            .expect("the provider is registered");
        scope.teardown().await;
        assert_eq!(counter.torn_down.load(Ordering::SeqCst), 1);

        let error = scope
            .exec(ExecSpec::new("sh", []), CancellationToken::new())
            .await
            .expect_err("a released scope must not prepare a second environment");
        assert_eq!(error.kind(), ErrorKind::Cancelled);

        // Idempotent: a second teardown neither panics nor releases twice.
        scope.teardown().await;
        assert_eq!(counter.torn_down.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn preparation_happens_once_and_only_when_a_process_is_run() {
        let counter = Arc::new(CountingHandle::default());
        let registry = registry_with(counter.clone()).await;
        let scope = SandboxScope::resolve(&registry, Some("counting"), request()).await;

        // Nothing prepared yet: most calls never spawn, and `prepare` is not free.
        scope.teardown().await;
        assert_eq!(
            counter.torn_down.load(Ordering::SeqCst),
            0,
            "an idle scope has nothing to release"
        );
    }
}
