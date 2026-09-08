//! Step 7 and the ownership that makes "no zombies after a cancel" true.
//!
//! Two properties, and they pull in opposite directions:
//!
//! - a call that never starts a process is **not** blocked for want of a sandbox, which is
//!   what keeps `--profile production` and every hand-written `enabled` list working; and
//! - a call that does start one either gets the confinement the decision named or fails
//!   naming it.
//!
//! The third is the dispatcher's, not the provider's: a tool task that ignores cancellation
//! is abandoned rather than aborted, so the *scope* is torn down by whoever is left holding
//! it — which is the dispatcher.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use rivet_core::event::{Event, ToolEvent};
use rivet_core::id::SandboxId;
use rivet_core::policy::{ExecutionConstraints, PolicyDecision};
use rivet_core::sandbox::{
    ExecOutput, ExecSpec, Sandbox, SandboxGuarantees, SandboxHandle, SandboxRequest,
};
use rivet_core::tool::{Tool, ToolContext, ToolResult, ToolSpec};
use rivet_runtime::agent_loop::{AgentLoop, RunConfig};
use rivet_runtime::jitter::NoJitter;
use support::{EchoTool, FixedPolicy, FixtureModel, Harness, Reply, sse_text, sse_tool_calls};
use tokio_util::sync::CancellationToken;

fn agent_loop(harness: &Harness) -> AgentLoop {
    AgentLoop::new(
        harness.registry.clone(),
        harness.store.clone(),
        harness.bus.clone(),
        harness.assembler(),
        Arc::new(rivet_core::retry::ExponentialBackoff::default()),
        Arc::new(NoJitter),
    )
}

fn config(harness: &Harness) -> RunConfig {
    let mut cfg = RunConfig::new(
        harness.agent(),
        harness.session_id,
        harness.workspace.clone(),
    );
    cfg.cancel_grace = Duration::from_millis(200);
    cfg
}

// --- doubles ---------------------------------------------------------------------------

/// Counts preparations and teardowns.
#[derive(Debug, Default)]
struct Counters {
    prepared: AtomicUsize,
    torn_down: AtomicUsize,
}

#[derive(Debug)]
struct CountingSandbox {
    counters: Arc<Counters>,
    name: String,
}

#[async_trait]
impl Sandbox for CountingSandbox {
    fn name(&self) -> &str {
        &self.name
    }

    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees::default()
    }

    async fn prepare(
        &self,
        _request: SandboxRequest,
    ) -> rivet_core::Result<Box<dyn SandboxHandle>> {
        self.counters.prepared.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CountingHandle {
            counters: self.counters.clone(),
            id: SandboxId::new(),
            name: self.name.clone(),
        }))
    }
}

#[derive(Debug)]
struct CountingHandle {
    counters: Arc<Counters>,
    id: SandboxId,
    name: String,
}

#[async_trait]
impl SandboxHandle for CountingHandle {
    fn id(&self) -> SandboxId {
        self.id
    }

    async fn exec(
        &self,
        _spec: ExecSpec,
        _cancel: CancellationToken,
    ) -> rivet_core::Result<ExecOutput> {
        Ok(ExecOutput {
            exit_code: Some(0),
            // Which provider ran it, so a test can tell two apart.
            stdout: self.name.clone(),
            stderr: String::new(),
            timed_out: false,
            truncated: false,
            duration_ms: 1,
        })
    }

    async fn teardown(&self) -> rivet_core::Result<()> {
        self.counters.torn_down.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A tool that starts a process through the host, and reports which provider answered.
#[derive(Debug)]
struct SpawningTool;

#[async_trait]
impl Tool for SpawningTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "spawn",
            "runs a process through the host",
            serde_json::json!({ "type": "object", "additionalProperties": false }),
        )
        .unwrap()
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        let output = ctx.host.exec(ExecSpec::new("true", [])).await?;
        Ok(ToolResult::ok(output.stdout))
    }
}

/// Ignores cancellation and never returns, but does start a process first.
#[derive(Debug)]
struct AbandonedSpawningTool {
    /// Tripped once the process is running, so the test cancels at the right moment.
    started: CancellationToken,
}

#[async_trait]
impl Tool for AbandonedSpawningTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "spawn",
            "starts a process and then ignores cancellation",
            serde_json::json!({ "type": "object", "additionalProperties": false }),
        )
        .unwrap()
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        ctx.host.exec(ExecSpec::new("true", [])).await?;
        self.started.cancel();
        tokio::time::sleep(Duration::from_secs(600)).await;
        Ok(ToolResult::ok("never"))
    }
}

/// Starts a process, then panics — so the scope is `Prepared` when the panic happens.
///
/// The distinction matters. A tool that panics *before* touching the host leaves the scope
/// `Idle`, and `teardown` on an idle scope releases nothing, so a test built on the harness's
/// plain `PanickingTool` would pass whether or not the panic path reached teardown at all.
#[derive(Debug)]
struct SpawningThenPanickingTool;

#[async_trait]
impl Tool for SpawningThenPanickingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "spawn",
            "starts a process and then panics",
            serde_json::json!({ "type": "object", "additionalProperties": false }),
        )
        .unwrap()
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        ctx.host.exec(ExecSpec::new("true", [])).await?;
        panic!("this tool is broken, and it had already started something");
    }
}

fn one_call(name: &str) -> Arc<FixtureModel> {
    Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_tool_calls(&[(name, serde_json::json!({}))])),
        Reply::Sse(sse_text("done")),
    ]))
}

/// The `sandboxed` flag on the one `tool.execute.started` a run published.
fn sandboxed_flag(recorder: &support::Recorder) -> bool {
    recorder
        .envelopes()
        .iter()
        .find_map(|envelope| match &envelope.payload {
            Event::Tool(ToolEvent::Started { sandboxed, .. }) => Some(*sandboxed),
            _ => None,
        })
        .expect("a call started")
}

// --- a call that never spawns ---------------------------------------------------------------

#[tokio::test]
async fn a_call_that_never_spawns_is_not_blocked_by_a_missing_sandbox() {
    // This is the test that keeps `--profile production` and every existing `rivet.toml`
    // with a hand-written `enabled` list working. Neither registers a sandbox provider, and
    // neither needs one: the file tools open files, they do not start processes.
    let harness = Harness::new().await;
    let (recorder, observer) = harness.recorder();
    harness
        .register_model(Arc::new(FixtureModel::new(vec![
            Reply::Sse(sse_tool_calls(&[(
                "echo",
                serde_json::json!({ "message": "hello" }),
            )])),
            Reply::Sse(sse_text("done")),
        ])))
        .await;
    harness.register_tool(Arc::new(EchoTool)).await;

    let mut cfg = config(&harness);
    cfg.sandbox_provider = Some("local".to_string());
    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");
    observer.drain_within(Duration::from_secs(2)).await;

    let topics = harness.topics().await;
    assert!(
        topics.contains(&"tool.completed".to_string()),
        "the call ran with no provider registered: {topics:?}"
    );
    assert!(!topics.contains(&"tool.blocked".to_string()));
    assert!(
        !sandboxed_flag(&recorder),
        "and the event says honestly that nothing confined it"
    );
}

#[tokio::test]
async fn a_call_that_spawns_without_a_sandbox_fails_naming_the_provider() {
    // The other side of the invariant. The failure lands where a process is actually asked
    // for -- not at step 7, which every call passes through.
    let harness = Harness::new().await;
    harness.register_model(one_call("spawn")).await;
    harness.register_tool(Arc::new(SpawningTool)).await;

    let mut cfg = config(&harness);
    cfg.sandbox_provider = Some("docker".to_string());
    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");

    let result = harness
        .events()
        .await
        .iter()
        .find_map(|stored| match &stored.event {
            rivet_core::session::SessionEvent::ToolCompleted { result, .. } => Some(result.clone()),
            _ => None,
        })
        .expect("a completed call");
    assert!(result.is_error);
    assert!(
        result.content.contains("docker"),
        "the failure has to name what it could not find: {}",
        result.content
    );
}

#[tokio::test]
async fn a_registered_provider_runs_the_process_and_is_reported() {
    let harness = Harness::new().await;
    let (recorder, observer) = harness.recorder();
    let counters = Arc::new(Counters::default());
    harness
        .register_sandbox(Arc::new(CountingSandbox {
            counters: counters.clone(),
            name: "local".into(),
        }))
        .await;
    harness.register_model(one_call("spawn")).await;
    harness.register_tool(Arc::new(SpawningTool)).await;

    let mut cfg = config(&harness);
    cfg.sandbox_provider = Some("local".to_string());
    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");
    observer.drain_within(Duration::from_secs(2)).await;

    assert!(sandboxed_flag(&recorder));
    assert_eq!(counters.prepared.load(Ordering::SeqCst), 1);
    assert_eq!(
        counters.torn_down.load(Ordering::SeqCst),
        1,
        "every path out of a call releases its sandbox"
    );
}

#[tokio::test]
async fn a_policy_named_provider_beats_the_configured_default() {
    // The one axis where the seed is *not* the host's: provider names have no order, so the
    // configured one is a default that a policy overrides rather than a floor it cannot
    // reach under.
    let harness = Harness::new().await;
    let counters = Arc::new(Counters::default());
    for name in ["local", "strict"] {
        harness
            .register_sandbox(Arc::new(CountingSandbox {
                counters: counters.clone(),
                name: name.into(),
            }))
            .await;
    }
    harness.register_model(one_call("spawn")).await;
    harness.register_tool(Arc::new(SpawningTool)).await;
    harness
        .register_policy(Arc::new(FixedPolicy::new(
            "a.wants_strict",
            PolicyDecision::allow().with_constraints(ExecutionConstraints {
                sandbox: Some("strict".into()),
                ..ExecutionConstraints::default()
            }),
        )))
        .await;

    let mut cfg = config(&harness);
    cfg.sandbox_provider = Some("local".to_string());
    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");

    let result = harness
        .events()
        .await
        .iter()
        .find_map(|stored| match &stored.event {
            rivet_core::session::SessionEvent::ToolCompleted { result, .. } => Some(result.clone()),
            _ => None,
        })
        .expect("a completed call");
    assert_eq!(
        result.content, "strict",
        "the policy named a provider and the configuration did not overrule it"
    );
}

#[tokio::test]
async fn an_abandoned_tool_task_still_gets_its_sandbox_torn_down() {
    // The whole of "no zombies after a cancel", and it is a property of *ownership* rather
    // than of the provider. The tool ignores cancellation, so the dispatcher gives up on it
    // and walks away -- with the scope still in hand.
    let harness = Harness::new().await;
    let counters = Arc::new(Counters::default());
    harness
        .register_sandbox(Arc::new(CountingSandbox {
            counters: counters.clone(),
            name: "local".into(),
        }))
        .await;
    harness.register_model(one_call("spawn")).await;

    let started = CancellationToken::new();
    harness
        .register_tool(Arc::new(AbandonedSpawningTool {
            started: started.clone(),
        }))
        .await;

    let cancel = CancellationToken::new();
    let mut cfg = config(&harness);
    cfg.sandbox_provider = Some("local".to_string());
    cfg.cancel = cancel.clone();

    let stopper = tokio::spawn(async move {
        started.cancelled().await;
        cancel.cancel();
    });

    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes even though the tool never does");
    stopper.await.expect("the stopper joins");

    assert_eq!(counters.prepared.load(Ordering::SeqCst), 1);
    assert_eq!(
        counters.torn_down.load(Ordering::SeqCst),
        1,
        "the abandoned task kept the handle alive; the dispatcher had to release it anyway"
    );
}

#[tokio::test]
async fn a_panicking_tool_that_had_started_a_process_still_gets_its_sandbox_torn_down() {
    // The last corner of "no zombies". Cancellation is covered by the abandoned-task test
    // above; this is the other way a tool leaves without returning.
    //
    // Worth being precise about which mechanism catches it: a panicking tool is
    // `tokio::spawn`ed, so its panic becomes a `JoinError` rather than an unwind through the
    // dispatcher. The `catch_unwind` around steps 8 and 9 guards the dispatcher's *own*
    // code. Both routes end at the same `teardown()`, and this test does not care which one
    // ran -- only that the process the tool started was released.
    let harness = Harness::new().await;
    let counters = Arc::new(Counters::default());
    harness
        .register_sandbox(Arc::new(CountingSandbox {
            counters: counters.clone(),
            name: "local".into(),
        }))
        .await;
    harness.register_model(one_call("spawn")).await;
    harness
        .register_tool(Arc::new(SpawningThenPanickingTool))
        .await;

    let mut cfg = config(&harness);
    cfg.sandbox_provider = Some("local".to_string());
    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("one broken tool does not take the run with it");

    assert_eq!(
        counters.prepared.load(Ordering::SeqCst),
        1,
        "the tool did start a process before panicking, or this proves nothing"
    );
    assert_eq!(
        counters.torn_down.load(Ordering::SeqCst),
        1,
        "the panic left without returning, and the dispatcher released the scope anyway"
    );
    assert!(
        harness
            .topics()
            .await
            .contains(&"tool.completed".to_string()),
        "and the call still reached a durable conclusion"
    );
}

#[tokio::test]
async fn a_tool_that_panics_before_spawning_leaves_nothing_behind() {
    // The harness's own `PanickingTool`, which never touches the host. There is nothing to
    // release here -- the scope is `Idle` -- and that is the assertion: teardown on an idle
    // scope is a no-op rather than an error, so the panic path costs nothing extra and
    // `SandboxScope::drop` has no reason to log.
    let harness = Harness::new().await;
    let counters = Arc::new(Counters::default());
    harness
        .register_sandbox(Arc::new(CountingSandbox {
            counters: counters.clone(),
            name: "local".into(),
        }))
        .await;
    harness.register_model(one_call("panics")).await;
    harness
        .register_tool(Arc::new(support::PanickingTool))
        .await;

    let mut cfg = config(&harness);
    cfg.sandbox_provider = Some("local".to_string());
    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run survives");

    assert_eq!(counters.prepared.load(Ordering::SeqCst), 0);
    assert_eq!(counters.torn_down.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_grant_the_chain_settled_on_is_what_the_sandbox_is_asked_to_enforce() {
    // A tool and its confinement have to read the same grant, or the confinement is
    // enforcing something other than what was decided.
    #[derive(Debug)]
    struct GrantRecording(Arc<std::sync::Mutex<Option<rivet_core::capability::PermissionSet>>>);

    #[async_trait]
    impl Sandbox for GrantRecording {
        fn name(&self) -> &'static str {
            "recording"
        }

        fn guarantees(&self) -> SandboxGuarantees {
            SandboxGuarantees::default()
        }

        async fn prepare(
            &self,
            request: SandboxRequest,
        ) -> rivet_core::Result<Box<dyn SandboxHandle>> {
            *self.0.lock().unwrap() = Some(request.permissions.clone());
            Ok(Box::new(CountingHandle {
                counters: Arc::new(Counters::default()),
                id: SandboxId::new(),
                name: "recording".into(),
            }))
        }
    }

    use rivet_core::capability::{FsScope, Permission, PermissionSet};
    let seen = Arc::new(std::sync::Mutex::new(None));
    let harness = Harness::new().await;
    harness
        .register_sandbox(Arc::new(GrantRecording(seen.clone())))
        .await;
    harness.register_model(one_call("spawn")).await;
    harness.register_tool(Arc::new(SpawningTool)).await;
    harness
        .register_policy(Arc::new(FixedPolicy::new(
            "a.narrows_the_grant",
            PolicyDecision::allow().with_constraints(ExecutionConstraints {
                permissions: Some(PermissionSet::new([Permission::FsRead(FsScope::Workspace)])),
                ..ExecutionConstraints::default()
            }),
        )))
        .await;

    let mut cfg = config(&harness);
    cfg.permissions = PermissionSet::new([
        Permission::FsRead(FsScope::Workspace),
        Permission::FsWrite(FsScope::Workspace),
        Permission::ProcessSpawn,
    ]);
    cfg.sandbox_provider = Some("recording".to_string());
    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");

    let granted = seen.lock().unwrap().clone().expect("the sandbox prepared");
    assert!(granted.allows(&Permission::FsRead(FsScope::Workspace)));
    assert!(
        !granted.allows(&Permission::FsWrite(FsScope::Workspace)),
        "the policy narrowed the grant, and the confinement is enforcing the narrowed one"
    );
}
