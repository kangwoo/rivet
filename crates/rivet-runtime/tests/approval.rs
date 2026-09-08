//! Step 6: what a person is asked, what a rule answers instead, and what the log keeps.
//!
//! Three of these use a sink that **panics if it is called**. A counting spy would let a
//! test pass while the run sat waiting for an answer; a panicking one fails on the first
//! wrong question, which is the property under test in every case: `--headless` does not
//! ask, and a remembered grant does not ask again.

mod support;

use std::sync::Arc;
use std::time::Duration;

use rivet_core::policy::ApprovalOutcome;
use rivet_core::session::SessionEvent;
use rivet_runtime::agent_loop::{AgentLoop, RunConfig};
use rivet_runtime::jitter::NoJitter;
use support::{
    EchoTool, FixedPolicy, FixtureModel, ForbiddenSink, Harness, Reply, SpySink, sse_text,
    sse_tool_calls,
};

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

/// A model that asks for `echo` once, then answers.
fn one_echo() -> Arc<FixtureModel> {
    Arc::new(FixtureModel::new(echo_then_answer(1)))
}

/// `runs` rounds of "call `echo`, then answer", for the tests that resume.
///
/// The script is per *model*, not per run, so a second run against the same harness reads
/// on from where the first stopped. Running dry is not an error — the fixture falls back to
/// a plain answer — which would quietly turn "the second run made no call" into a pass.
fn echo_then_answer(runs: usize) -> Vec<Reply> {
    (0..runs)
        .flat_map(|round| {
            [
                Reply::Sse(sse_tool_calls(&[(
                    "echo",
                    serde_json::json!({ "message": format!("hello {round}") }),
                )])),
                Reply::Sse(sse_text("done")),
            ]
        })
        .collect()
}

/// A harness with a tool, a model, and a policy that asks about every call.
async fn asking_harness(scope_key: &str, allow_remember: bool) -> Harness {
    let harness = Harness::new().await;
    harness.register_model(one_echo()).await;
    harness.register_tool(Arc::new(EchoTool)).await;
    harness
        .register_policy(Arc::new(FixedPolicy::asking(
            "default.destructive",
            scope_key,
            allow_remember,
        )))
        .await;
    harness
}

/// The `(outcome, remembered, actor)` of every `approval.resolved` in the log.
async fn resolutions(harness: &Harness) -> Vec<(ApprovalOutcome, bool, Option<String>)> {
    harness
        .events()
        .await
        .iter()
        .filter_map(|stored| match &stored.event {
            SessionEvent::ApprovalResolved {
                outcome,
                remembered,
                actor,
                ..
            } => Some((outcome.clone(), *remembered, actor.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_headless_run_denies_an_approval_instead_of_waiting() {
    // The first acceptance line. The sink panics rather than counts: a run that *waited* on
    // it would hang, and a hang is not a failure a counting assertion can catch.
    let harness = asking_harness("echo", true).await;
    let mut cfg = config(&harness);
    cfg.unattended = true;
    cfg.approval_sink = Some(Arc::new(ForbiddenSink));

    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes rather than hanging");

    let topics = harness.topics().await;
    assert!(topics.contains(&"approval.requested".to_string()));
    assert!(topics.contains(&"approval.resolved".to_string()));
    assert!(
        topics.contains(&"tool.blocked".to_string()),
        "and the call did not run: {topics:?}"
    );
    assert!(!topics.contains(&"tool.called".to_string()));

    assert_eq!(
        resolutions(&harness).await,
        [(ApprovalOutcome::Denied, false, None)],
        "a rule answered, so there is nobody to name"
    );
}

#[tokio::test]
async fn an_approval_leaves_a_requested_and_a_resolved_in_the_log() {
    // Durable, in order, and around the call rather than after it: the approval decides
    // whether `tool.called` happens at all.
    let harness = asking_harness("echo", false).await;
    let sink = SpySink::answering([ApprovalOutcome::Approved]);
    let mut cfg = config(&harness);
    cfg.approval_sink = Some(sink.clone());

    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");

    // Both halves of the pair, durably, in order, and around the call rather than after
    // it -- the approval is what decides whether `tool.called` happens at all. This is the
    // test `docs/plan.md` cites for that DoD line, so it asserts the request as well as the
    // resolution rather than leaving one half to a neighbouring test.
    let topics = harness.topics().await;
    let requested = topics
        .iter()
        .position(|t| t == "approval.requested")
        .expect("the request is durable, not only the answer");
    let approved = topics
        .iter()
        .position(|t| t == "approval.resolved")
        .expect("a resolution");
    let called = topics
        .iter()
        .position(|t| t == "tool.called")
        .expect("the call ran");
    assert!(
        requested < approved && approved < called,
        "requested, then resolved, then called: {topics:?}"
    );

    // And what the request carried, from the policy that asked.
    let asked_for = harness
        .events()
        .await
        .iter()
        .find_map(|stored| match &stored.event {
            SessionEvent::ApprovalRequested {
                reason,
                preview,
                scope_key,
                ..
            } => Some((reason.clone(), preview.clone(), scope_key.clone())),
            _ => None,
        })
        .expect("an approval request");
    assert_eq!(asked_for.2, "echo", "the scope key the policy chose");
    assert!(
        asked_for.1.contains("echo"),
        "the preview says what will happen: {}",
        asked_for.1
    );
    assert!(!asked_for.0.is_empty(), "and why it is being asked");

    let resolved = resolutions(&harness).await;
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].0, ApprovalOutcome::Approved);
    assert!(
        resolved[0].2.is_some(),
        "a person answered, so the log names one"
    );

    // And the sink saw what the policy actually said.
    let asked = sink.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].scope_key, "echo");
    assert!(asked[0].preview.contains("echo"), "{:?}", asked[0]);
}

#[tokio::test]
async fn a_refusal_blocks_the_call_and_names_the_policy() {
    let harness = asking_harness("echo", false).await;
    let mut cfg = config(&harness);
    cfg.approval_sink = Some(SpySink::answering([ApprovalOutcome::Denied]));

    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");

    let blocked = harness
        .events()
        .await
        .iter()
        .find_map(|stored| match &stored.event {
            SessionEvent::ToolBlocked { policy, .. } => Some(policy.clone()),
            _ => None,
        })
        .expect("a refusal is durable");
    assert_eq!(blocked, "default.destructive");
}

#[tokio::test]
async fn an_approval_with_no_sink_is_denied_even_when_attended() {
    // The CLI never builds this combination -- `run.rs` folds a missing sink into
    // `unattended` itself -- but an embedder using `rivet-runtime` directly can, and an
    // approval with nowhere to go must not pass quietly.
    let harness = asking_harness("echo", true).await;
    let mut cfg = config(&harness);
    cfg.unattended = false;
    cfg.approval_sink = None;

    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");

    assert_eq!(
        resolutions(&harness).await,
        [(ApprovalOutcome::Denied, false, None)]
    );
    assert!(!harness.topics().await.contains(&"tool.called".to_string()));
}

#[tokio::test]
async fn a_remembered_grant_is_written_once_and_only_once() {
    // `SessionState::apply` pushes on `remembered` without folding duplicates, so a second
    // event claiming to create the same grant would grow the projection on every call.
    let harness = Harness::new().await;
    harness
        .register_model(Arc::new(FixtureModel::new(vec![
            Reply::Sse(sse_tool_calls(&[(
                "echo",
                serde_json::json!({ "message": "one" }),
            )])),
            Reply::Sse(sse_tool_calls(&[(
                "echo",
                serde_json::json!({ "message": "two" }),
            )])),
            Reply::Sse(sse_text("done")),
        ])))
        .await;
    harness.register_tool(Arc::new(EchoTool)).await;
    harness
        .register_policy(Arc::new(FixedPolicy::asking(
            "default.destructive",
            "echo",
            true,
        )))
        .await;

    let sink = SpySink::answering([ApprovalOutcome::ApprovedForSession]);
    let mut cfg = config(&harness);
    cfg.approval_sink = Some(sink.clone());

    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");

    assert_eq!(
        sink.asked().len(),
        1,
        "the second call was covered by the grant the first one made"
    );
    let resolved = resolutions(&harness).await;
    assert_eq!(resolved.len(), 2, "both calls are still recorded");
    assert_eq!(
        resolved[0],
        (
            ApprovalOutcome::ApprovedForSession,
            true,
            resolved[0].2.clone()
        )
    );
    assert!(resolved[0].2.is_some(), "a person made the grant");
    assert_eq!(
        resolved[1],
        (ApprovalOutcome::Approved, false, None),
        "the second is a *use* of the grant, not another grant"
    );

    assert_eq!(
        harness.state().await.remembered_approvals(),
        ["echo"],
        "one grant, not one per call"
    );
}

#[tokio::test]
async fn a_replayed_grant_skips_the_sink_on_the_next_run() {
    // The acceptance line about resume, and the only way to prove it: run 1 makes the grant,
    // the log is replayed into a fresh `SessionState`, and run 2 starts from that state with
    // a sink that panics if anything asks.
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(EchoTool)).await;
    harness
        .register_policy(Arc::new(FixedPolicy::asking(
            "default.destructive",
            "echo",
            true,
        )))
        .await;

    harness
        .register_model(Arc::new(FixtureModel::new(echo_then_answer(2))))
        .await;
    let mut first = config(&harness);
    first.approval_sink = Some(SpySink::answering([ApprovalOutcome::ApprovedForSession]));
    agent_loop(&harness)
        .run(first, harness.state().await, None)
        .await
        .expect("run 1 finishes");

    // A fresh process would rebuild the conversation exactly this way.
    let replayed = harness.state().await;
    assert_eq!(replayed.remembered_approvals(), ["echo"]);
    assert!(replayed.is_pre_approved("echo"));

    let mut second = config(&harness);
    second.run_id = rivet_core::id::RunId::new();
    second.approval_sink = Some(Arc::new(ForbiddenSink));
    agent_loop(&harness)
        .run(second, replayed, None)
        .await
        .expect("run 2 finishes without asking anybody");

    let called = harness
        .topics()
        .await
        .iter()
        .filter(|t| *t == "tool.called")
        .count();
    assert_eq!(called, 2, "the second run's call ran on the earlier grant");
}

#[tokio::test]
async fn a_remembered_approval_is_honored_even_when_unattended() {
    // The order in step 6: memory first, then the unattended conversion. `unattended` means
    // "nobody can answer now"; a remembered grant means "somebody already did", and a flag
    // on today's run must not throw away a decision a person made durably. Reversing the two
    // is defensible and more closed -- and it would make the resume line true only of
    // attended resumes, which is narrower than the line says.
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(EchoTool)).await;
    harness
        .register_policy(Arc::new(FixedPolicy::asking(
            "default.destructive",
            "echo",
            true,
        )))
        .await;
    harness
        .register_model(Arc::new(FixtureModel::new(echo_then_answer(2))))
        .await;

    let mut first = config(&harness);
    first.approval_sink = Some(SpySink::answering([ApprovalOutcome::ApprovedForSession]));
    agent_loop(&harness)
        .run(first, harness.state().await, None)
        .await
        .expect("run 1 finishes");

    let mut headless = config(&harness);
    headless.run_id = rivet_core::id::RunId::new();
    headless.unattended = true;
    headless.approval_sink = None;
    agent_loop(&harness)
        .run(headless, harness.state().await, None)
        .await
        .expect("run 2 finishes");

    let resolved = resolutions(&harness).await;
    assert_eq!(
        resolved.last().map(|r| r.0.clone()),
        Some(ApprovalOutcome::Approved),
        "`--headless` did not erase the grant: {resolved:?}"
    );
}

/// Keys the approval on the call's own message, so two calls ask about two things.
#[derive(Debug)]
struct PerMessagePolicy;

#[async_trait::async_trait]
impl rivet_core::policy::Policy for PerMessagePolicy {
    fn name(&self) -> &'static str {
        "test.per_message"
    }

    async fn evaluate(
        &self,
        request: &rivet_core::policy::PolicyRequest,
    ) -> rivet_core::Result<rivet_core::policy::PolicyDecision> {
        let rivet_core::policy::PolicyAction::ToolCall { call, .. } = &request.action else {
            return Ok(rivet_core::policy::PolicyDecision::allow());
        };
        let message = call.input["message"].as_str().unwrap_or_default();
        Ok(rivet_core::policy::PolicyDecision {
            outcome: rivet_core::policy::Outcome::RequireApproval {
                reason: "the test asked for one".into(),
                preview: format!("echo {message}"),
                allow_remember: true,
                scope_key: format!("echo:{message}"),
            },
            rewrite: None,
            constraints: rivet_core::policy::ExecutionConstraints::default(),
        })
    }
}

#[tokio::test]
async fn a_remembered_approval_does_not_cover_a_different_scope_key() {
    // A grant is over a `scope_key`, not over the session. Otherwise "allow this for the
    // session" would quietly be "stop asking me about anything".
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(EchoTool)).await;
    harness.register_policy(Arc::new(PerMessagePolicy)).await;
    harness
        .register_model(Arc::new(FixtureModel::new(vec![
            Reply::Sse(sse_tool_calls(&[(
                "echo",
                serde_json::json!({ "message": "one" }),
            )])),
            Reply::Sse(sse_tool_calls(&[(
                "echo",
                serde_json::json!({ "message": "two" }),
            )])),
            Reply::Sse(sse_text("done")),
        ])))
        .await;

    // The first call is granted for the session; the second is about a different key.
    let sink = SpySink::answering([ApprovalOutcome::ApprovedForSession, ApprovalOutcome::Denied]);
    let mut cfg = config(&harness);
    cfg.approval_sink = Some(sink.clone());

    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");

    let asked: Vec<String> = sink.asked().iter().map(|r| r.scope_key.clone()).collect();
    assert_eq!(
        asked,
        ["echo:one", "echo:two"],
        "the grant over one key did not answer for the other"
    );
    assert_eq!(harness.state().await.remembered_approvals(), ["echo:one"]);
}

#[tokio::test]
async fn the_bus_carries_both_approval_topics() {
    // The tripwire in `event_flow.rs` only asserts that these topics *claim* a publisher.
    // This is the claim being true.
    let harness = asking_harness("echo", false).await;
    let (recorder, observer) = harness.recorder();
    let mut cfg = config(&harness);
    cfg.approval_sink = Some(SpySink::answering([ApprovalOutcome::Approved]));

    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");
    observer.drain_within(Duration::from_secs(2)).await;

    let topics = recorder.topics();
    assert!(
        topics.contains(&"tool.approval.requested".to_string()),
        "{topics:?}"
    );
    assert!(
        topics.contains(&"tool.approval.resolved".to_string()),
        "{topics:?}"
    );
}
