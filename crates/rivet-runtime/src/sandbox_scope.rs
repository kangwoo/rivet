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
//! `prepare` is held to the same rule, and by the same mechanism: "a preparation is in
//! flight" is a *state* rather than "the lock is held". Awaiting a provider under the mutex
//! left the child covered and the preparation not — a tool abandoned during its first `exec`
//! would leave the dispatcher's `teardown()` waiting on a `prepare` that takes no
//! cancellation token. Nothing was broken, because `LocalSandbox::prepare` has no `await` in
//! it at all; but the point of hoisting the `?` above the scope was that "teardown on every
//! path" be a property of the shape rather than a fact about another module, and this was
//! the one place the shape did not carry it. A scope sealed while a `prepare` is in flight tears the fresh
//! handle down itself, so nothing is left running with nobody to stop it.
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
    /// A `prepare` is in flight, and the lock is *not* held while it runs.
    ///
    /// Two things need this to be a state. `teardown` must be able to seal the scope while a
    /// provider is still preparing, and a second concurrent `exec` must wait rather than
    /// prepare an environment of its own.
    Preparing,
    Prepared(Arc<dyn SandboxHandle>),
    TornDown,
}

/// One call's confinement.
pub struct SandboxScope {
    name: Option<String>,
    provider: Option<Arc<dyn Sandbox>>,
    request: SandboxRequest,
    state: Mutex<ScopeState>,
    /// Woken every time the `Preparing` state is left, whichever way it is left.
    ///
    /// What a second concurrent `exec` waits on instead of the lock, so "prepare once"
    /// survives the lock being released across the provider's `await`.
    prepared: tokio::sync::Notify,
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
            prepared: tokio::sync::Notify::new(),
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
            prepared: tokio::sync::Notify::new(),
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
        let handle = self.handle(&spec).await?;
        // The lock is gone before the child is waited on, so `teardown` can run while a
        // process is still going — which is the whole point of holding this from outside
        // the tool task.
        handle.exec(spec, cancel).await
    }

    /// The prepared handle, preparing one on first use.
    ///
    /// The lock is held to read the state and to write it, never across the provider's
    /// `await`. `spec` is here for the message a missing provider gets, and nothing else.
    async fn handle(&self, spec: &ExecSpec) -> rivet_core::Result<Arc<dyn SandboxHandle>> {
        loop {
            // Registered *before* the state is read: a `notify_waiters` landing between the
            // read and the wait would otherwise be a wakeup this task never sees.
            let prepared = self.prepared.notified();
            let next = {
                let mut state = self.state.lock().await;
                match &mut *state {
                    ScopeState::TornDown => Next::Sealed,
                    ScopeState::Prepared(handle) => return Ok(handle.clone()),
                    ScopeState::Preparing => Next::Wait,
                    idle @ ScopeState::Idle => match self.provider.clone() {
                        Some(provider) => {
                            *idle = ScopeState::Preparing;
                            Next::Prepare(provider)
                        }
                        None => Next::Missing,
                    },
                }
            };
            match next {
                Next::Prepare(provider) => return self.prepare(provider).await,
                Next::Sealed => return Err(sealed()),
                Next::Missing => return Err(self.missing(spec)),
                Next::Wait => prepared.await,
            }
        }
    }

    /// Prepare the environment with the lock released, then publish the result.
    ///
    /// The caller left the scope in `Preparing`, so this owns the transition out of
    /// it and every way out has to take it — the failing one back to `Idle`, so a later call
    /// may try again rather than inherit a failure it did not cause.
    async fn prepare(
        &self,
        provider: Arc<dyn Sandbox>,
    ) -> rivet_core::Result<Arc<dyn SandboxHandle>> {
        let prepared = provider.prepare(self.request.clone()).await;
        let mut state = self.state.lock().await;
        let sealed_meanwhile = matches!(*state, ScopeState::TornDown);
        match prepared {
            Ok(handle) if !sealed_meanwhile => {
                let handle: Arc<dyn SandboxHandle> = Arc::from(handle);
                *state = ScopeState::Prepared(handle.clone());
                drop(state);
                self.prepared.notify_waiters();
                Ok(handle)
            }
            Ok(handle) => {
                // `teardown` looked while this was in flight, found `Preparing`, had nothing
                // to release and sealed the scope. Releasing what was just made is this
                // task's job; leaving it would be the orphan this module exists to prevent.
                drop(state);
                self.prepared.notify_waiters();
                if let Err(error) = handle.teardown().await {
                    tracing::error!(%error, "a sandbox did not release cleanly");
                }
                Err(sealed())
            }
            Err(error) => {
                if !sealed_meanwhile {
                    *state = ScopeState::Idle;
                }
                drop(state);
                self.prepared.notify_waiters();
                Err(error)
            }
        }
    }

    /// The error a call gets when the decision named a provider nothing registered.
    fn missing(&self, spec: &ExecSpec) -> Error {
        Error::new(
            ErrorKind::NotFound,
            Capability::Sandbox,
            match &self.name {
                Some(name) => format!(
                    "no sandbox provider named `{name}` is registered, and `{}` needs one to \
                     run a process",
                    spec.program
                ),
                None => format!(
                    "no sandbox provider is configured, and `{}` needs one to run a process",
                    spec.program
                ),
            },
        )
    }

    /// Release the environment, and seal the scope. Idempotent.
    ///
    /// Takes `&self` because an abandoned tool task still holds an `Arc` to this value, so
    /// the dispatcher cannot take ownership back to release it.
    ///
    /// A preparation still in flight is **not** waited for. The lock is free while a provider
    /// prepares, so this seals the scope at once and the preparing task releases whatever it
    /// ends up with; waiting instead would put back exactly the block this shape removes.
    pub async fn teardown(&self) {
        let prepared = {
            let mut state = self.state.lock().await;
            match std::mem::replace(&mut *state, ScopeState::TornDown) {
                ScopeState::Prepared(handle) => Some(handle),
                ScopeState::Idle | ScopeState::Preparing | ScopeState::TornDown => None,
            }
        };
        if let Some(handle) = prepared
            && let Err(error) = handle.teardown().await
        {
            tracing::error!(%error, "a sandbox did not release cleanly");
        }
    }
}

/// What [`SandboxScope::handle`] found the state to be.
///
/// A value rather than acting inside the `match`, so the lock is released before anything
/// that awaits.
enum Next {
    Prepare(Arc<dyn Sandbox>),
    Wait,
    Sealed,
    Missing,
}

/// The refusal a released scope gives.
fn sealed() -> Error {
    Error::cancelled(
        "this call's sandbox has already been released; \
         refusing to start another process under it",
    )
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
    use std::time::Duration;

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

    /// A provider whose `prepare` parks until the test lets it through.
    ///
    /// `entered` is handed one permit the moment `prepare` is reached, so a test can tell
    /// "in flight" from "not started yet" without sleeping; `gate` is what lets it finish.
    #[derive(Debug)]
    struct SlowToPrepare {
        handle: Arc<CountingHandle>,
        entered: Arc<tokio::sync::Semaphore>,
        gate: Arc<tokio::sync::Semaphore>,
        prepared: AtomicUsize,
    }

    #[async_trait]
    impl Sandbox for SlowToPrepare {
        fn name(&self) -> &'static str {
            "slow"
        }

        fn guarantees(&self) -> rivet_core::sandbox::SandboxGuarantees {
            rivet_core::sandbox::SandboxGuarantees::default()
        }

        async fn prepare(
            &self,
            _request: SandboxRequest,
        ) -> rivet_core::Result<Box<dyn SandboxHandle>> {
            self.entered.add_permits(1);
            self.gate
                .acquire()
                .await
                .expect("the gate outlives the preparation")
                .forget();
            self.prepared.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(CloneOf(self.handle.clone())))
        }
    }

    async fn registry_with(provider: Arc<CountingHandle>) -> Registry {
        registry_holding(Arc::new(OneHandle(provider))).await
    }

    async fn registry_holding(provider: Arc<dyn Sandbox>) -> Registry {
        use rivet_core::plugin::PluginRegistry;
        let registry = Registry::new(crate::bus::BroadcastBus::new());
        let owner = crate::registry::Owner {
            plugin_id: rivet_core::id::PluginId::new("rivet.sandbox-test").unwrap(),
            instance_id: rivet_core::id::PluginInstanceId::new(),
        };
        registry
            .scoped(owner)
            .register_sandbox(provider)
            .await
            .unwrap();
        registry
    }

    fn slow_provider(handle: &Arc<CountingHandle>) -> Arc<SlowToPrepare> {
        Arc::new(SlowToPrepare {
            handle: handle.clone(),
            entered: Arc::new(tokio::sync::Semaphore::new(0)),
            gate: Arc::new(tokio::sync::Semaphore::new(0)),
            prepared: AtomicUsize::new(0),
        })
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
    async fn teardown_does_not_wait_for_a_preparation_in_flight() {
        // The claim the module makes about the child, made about `prepare` too. Awaiting a
        // provider under `state`'s lock left the dispatcher's `teardown()` -- the one on
        // every path out of steps 7 to 9 -- blocked on a call that has no cancellation token
        // of its own.
        let counter = Arc::new(CountingHandle::default());
        let provider = slow_provider(&counter);
        let registry = registry_holding(provider.clone()).await;
        let scope = Arc::new(SandboxScope::resolve(&registry, Some("slow"), request()).await);

        let running = {
            let scope = scope.clone();
            tokio::spawn(async move {
                scope
                    .exec(ExecSpec::new("sh", []), CancellationToken::new())
                    .await
            })
        };
        provider
            .entered
            .acquire()
            .await
            .expect("the preparation starts")
            .forget();

        tokio::time::timeout(Duration::from_secs(5), scope.teardown())
            .await
            .expect("teardown must not wait on a preparation it cannot cancel");

        provider.gate.add_permits(1);
        let error = running
            .await
            .expect("the task joins")
            .expect_err("a scope sealed meanwhile starts no process");
        assert_eq!(error.kind(), ErrorKind::Cancelled);
        assert_eq!(
            counter.torn_down.load(Ordering::SeqCst),
            1,
            "the environment finished after the seal is released, not orphaned"
        );
    }

    #[tokio::test]
    async fn two_calls_racing_the_first_use_prepare_one_environment() {
        // "Prepare once" used to be the mutex's doing, and the mutex is no longer held
        // across the call. `Preparing` is what carries it now, and a second `exec` waits on
        // that rather than making an environment of its own.
        let counter = Arc::new(CountingHandle::default());
        let provider = slow_provider(&counter);
        let registry = registry_holding(provider.clone()).await;
        let scope = Arc::new(SandboxScope::resolve(&registry, Some("slow"), request()).await);

        let calls: Vec<_> = (0..2)
            .map(|_| {
                let scope = scope.clone();
                tokio::spawn(async move {
                    scope
                        .exec(ExecSpec::new("sh", []), CancellationToken::new())
                        .await
                })
            })
            .collect();
        provider
            .entered
            .acquire()
            .await
            .expect("one of them starts preparing")
            .forget();
        provider.gate.add_permits(1);

        for call in calls {
            call.await
                .expect("the task joins")
                .expect("both calls run under the one environment");
        }
        assert_eq!(
            provider.prepared.load(Ordering::SeqCst),
            1,
            "a second environment nobody asked for is a second thing to tear down"
        );
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
