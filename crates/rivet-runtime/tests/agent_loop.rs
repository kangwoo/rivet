//! The agent loop, end to end against a real session store.
//!
//! Covers the five limits (one test each), cancellation and its five-second budget,
//! retries, and the property that makes a cancelled run resumable: every tool call in an
//! abandoned turn still gets an answer.

mod support;

use std::sync::Arc;
use std::time::Duration;

use rivet_core::agent::{LimitKind, StopReason};
use rivet_core::retry::ExponentialBackoff;
use rivet_runtime::agent_loop::{AgentLoop, RunConfig};
use rivet_runtime::jitter::NoJitter;
use rivet_runtime::session_recovery::{self, Rli};
use support::{
    EchoTool, FailingStore, FailingTool, FixtureModel, Harness, PanickingTool, ReadTool, Reply,
    SlowTool, StubbornTool, replay, sse_text, sse_tool_calls, sse_truncated, tool_results_in,
    transient,
};

/// Build a loop over a harness.
fn agent_loop(harness: &Harness) -> AgentLoop {
    AgentLoop::new(
        harness.registry.clone(),
        harness.store.clone(),
        harness.bus.clone(),
        harness.assembler(),
        Arc::new(ExponentialBackoff {
            max_attempts: 3,
            base_delay_ms: 1,
            max_delay_ms: 5,
            factor: 2,
        }),
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

// --- the ordinary path -------------------------------------------------------------------

#[tokio::test]
async fn a_plain_answer_ends_the_run() {
    let harness = Harness::new().await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_text(
        "It is a Rust agent runtime.",
    ))]));
    harness.register_model(model.clone()).await;

    let summary = agent_loop(&harness)
        .run(
            config(&harness),
            harness.state().await,
            Some(rivet_core::model::Message::user("what is this?")),
        )
        .await
        .unwrap();

    assert_eq!(summary.stop, StopReason::EndTurn);
    assert!(summary.stop.is_success());
    assert_eq!(summary.turns, 1);
    assert_eq!(summary.usage.input_tokens, 10);

    assert_eq!(
        harness.topics().await,
        [
            "session.created",
            "run.started",
            "user.message",
            "model.requested",
            "assistant.message",
            "run.completed"
        ]
    );
}

#[tokio::test]
async fn a_tool_result_reaches_the_next_request() {
    // DoD 2. Not "a tool ran" -- that the *model* was shown what it produced.
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(ReadTool)).await;
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_tool_calls(&[(
            "read_file",
            serde_json::json!({ "path": "src/main.rs" }),
        )])),
        Reply::Sse(sse_text("It prints nothing.")),
    ]));
    harness.register_model(model.clone()).await;

    let summary = agent_loop(&harness)
        .run(
            config(&harness),
            harness.state().await,
            Some(rivet_core::model::Message::user("what does main do?")),
        )
        .await
        .unwrap();

    assert_eq!(summary.stop, StopReason::EndTurn);
    assert_eq!(summary.turns, 2);
    assert_eq!(summary.tool_calls, 1);

    let requests = model.requests().await;
    assert_eq!(requests.len(), 2);
    let results = tool_results_in(&requests[1]);
    assert_eq!(results.len(), 1);
    assert!(
        results[0].contains("fn main"),
        "the model must see the file it asked for: {results:?}"
    );

    assert!(
        harness.topics().await.contains(&"tool.called".to_string()),
        "and the call is a durable fact"
    );
}

#[tokio::test]
async fn the_in_memory_conversation_matches_a_replay_of_the_log() {
    // The loop mirrors tool results into its own state as it goes. If that mirror ever
    // drifts from what `SessionState::apply` projects, a resumed session and a live one
    // would disagree about what was said.
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(EchoTool)).await;
    harness.register_tool(Arc::new(FailingTool)).await;
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_tool_calls(&[
            ("echo", serde_json::json!({ "message": "hi" })),
            ("always_fails", serde_json::json!({})),
            ("nope", serde_json::json!({})),
        ])),
        Reply::Sse(sse_text("all done")),
    ]));
    harness.register_model(model.clone()).await;

    agent_loop(&harness)
        .run(
            config(&harness),
            harness.state().await,
            Some(rivet_core::model::Message::user("go")),
        )
        .await
        .unwrap();

    let from_log = replay(&harness.events().await);
    let last_request = model.requests().await.pop().expect("a second request");
    // The last request's messages are the assembled view of the same conversation.
    let logged: Vec<String> = tool_results_in(&last_request);
    let expected: Vec<String> = from_log
        .messages
        .iter()
        .filter(|m| m.role == rivet_core::model::Role::Tool)
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                rivet_core::model::ContentBlock::ToolResult { content, .. } => {
                    Some(content.clone())
                }
                _ => None,
            })
        })
        .collect();
    pretty_assertions::assert_eq!(logged, expected);
}

#[tokio::test]
async fn an_unknown_tool_comes_back_as_something_the_model_can_fix() {
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(EchoTool)).await;
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_tool_calls(&[("nope", serde_json::json!({}))])),
        Reply::Sse(sse_text("I will use echo instead.")),
    ]));
    harness.register_model(model.clone()).await;

    let summary = agent_loop(&harness)
        .run(config(&harness), harness.state().await, None)
        .await
        .unwrap();
    assert_eq!(summary.stop, StopReason::EndTurn);

    let results = tool_results_in(&model.requests().await[1]);
    assert!(results[0].contains("unknown tool `nope`"), "{results:?}");
    assert!(
        results[0].contains("echo"),
        "and it is told what it can use: {results:?}"
    );
}

#[tokio::test]
async fn a_panicking_tool_does_not_take_the_run_with_it() {
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(PanickingTool)).await;
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_tool_calls(&[("panics", serde_json::json!({}))])),
        Reply::Sse(sse_text("noted")),
    ]));
    harness.register_model(model.clone()).await;

    let summary = agent_loop(&harness)
        .run(config(&harness), harness.state().await, None)
        .await
        .unwrap();
    assert_eq!(summary.stop, StopReason::EndTurn);
    let results = tool_results_in(&model.requests().await[1]);
    assert!(results[0].contains("panicked"), "{results:?}");
}

#[tokio::test]
async fn a_tool_outside_the_agents_scope_is_blocked_and_recorded() {
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(EchoTool)).await;
    harness.register_tool(Arc::new(FailingTool)).await;
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_tool_calls(&[("always_fails", serde_json::json!({}))])),
        Reply::Sse(sse_text("understood")),
    ]));
    harness.register_model(model.clone()).await;

    let mut cfg = config(&harness);
    cfg.agent.tools = vec!["echo".into()];
    agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();

    assert!(
        harness.topics().await.contains(&"tool.blocked".to_string()),
        "a refusal is exactly the fact an audit needs"
    );
    let results = tool_results_in(&model.requests().await[1]);
    assert!(results[0].contains("Blocked by policy"), "{results:?}");
}

// --- the five limits ----------------------------------------------------------------------

#[tokio::test]
async fn the_turn_limit_trips() {
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(EchoTool)).await;
    // A model that asks for another tool forever. Without the limit this never stops.
    let model = Arc::new(FixtureModel::repeating(sse_tool_calls(&[(
        "echo",
        serde_json::json!({ "message": "again" }),
    )])));
    harness.register_model(model).await;

    let mut cfg = config(&harness);
    cfg.agent.limits.max_turns = 2;
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();

    assert_eq!(
        summary.stop,
        StopReason::LimitReached {
            limit: LimitKind::Turns
        }
    );
    assert_eq!(summary.turns, 2);
}

#[tokio::test]
async fn the_duration_limit_trips() {
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(SlowTool::default())).await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_tool_calls(&[(
        "slow",
        serde_json::json!({}),
    )]))]));
    harness.register_model(model).await;

    let mut cfg = config(&harness);
    // The tool blocks until it is cancelled, so the deadline is guaranteed to fire inside
    // the call -- the case a turn-boundary check alone would miss. The headroom is for the
    // round trip before it, not for the tool.
    cfg.agent.limits.max_duration_ms = 750;
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();

    assert_eq!(
        summary.stop,
        StopReason::LimitReached {
            limit: LimitKind::Duration
        },
        "a deadline that fires inside a tool call must still be reported as a deadline, \
         not as a cancellation"
    );
    assert!(summary.duration_ms < 5_000);
}

#[tokio::test]
async fn the_token_limit_trips() {
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(EchoTool)).await;
    // Each round trip reports 15 tokens; the run is allowed 20 in total.
    let model = Arc::new(FixtureModel::repeating(sse_tool_calls(&[(
        "echo",
        serde_json::json!({ "message": "x" }),
    )])));
    harness.register_model(model).await;

    let mut cfg = config(&harness);
    cfg.agent.limits.max_total_tokens = 20;
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();

    assert_eq!(
        summary.stop,
        StopReason::LimitReached {
            limit: LimitKind::Tokens
        },
        "one typo must not be able to spend an unbounded budget"
    );
    assert!(summary.usage.total() >= 20);
}

#[tokio::test]
async fn the_context_limit_trips() {
    let harness = Harness::new().await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_text("hi"))]));
    harness.register_model(model).await;

    let mut cfg = config(&harness);
    // The system prompt alone is larger than this.
    cfg.agent.limits.max_context_tokens = 5;
    let summary = agent_loop(&harness)
        .run(
            cfg,
            harness.state().await,
            Some(rivet_core::model::Message::user("hello")),
        )
        .await
        .unwrap();

    assert_eq!(
        summary.stop,
        StopReason::LimitReached {
            limit: LimitKind::ContextSize
        },
        "better to say the request cannot be built than to send a broken one"
    );
}

#[tokio::test]
async fn the_consecutive_tool_error_limit_trips() {
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(FailingTool)).await;
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_tool_calls(&[("always_fails", serde_json::json!({}))])),
        Reply::Sse(sse_tool_calls(&[("always_fails", serde_json::json!({}))])),
        Reply::Sse(sse_tool_calls(&[("always_fails", serde_json::json!({}))])),
    ]));
    harness.register_model(model).await;

    let mut cfg = config(&harness);
    cfg.agent.limits.max_consecutive_tool_errors = 2;
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();

    assert_eq!(
        summary.stop,
        stop_reached_consecutive(),
        "retrying the same broken call forty times is the pattern this exists to catch"
    );
    assert_eq!(summary.tool_errors, 2);
}

fn stop_reached_consecutive() -> StopReason {
    StopReason::LimitReached {
        limit: LimitKind::ConsecutiveToolErrors,
    }
}

#[tokio::test]
async fn a_success_resets_the_consecutive_error_counter() {
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(FailingTool)).await;
    harness.register_tool(Arc::new(EchoTool)).await;
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_tool_calls(&[("always_fails", serde_json::json!({}))])),
        Reply::Sse(sse_tool_calls(&[(
            "echo",
            serde_json::json!({ "message": "recovered" }),
        )])),
        Reply::Sse(sse_tool_calls(&[("always_fails", serde_json::json!({}))])),
        Reply::Sse(sse_text("giving up")),
    ]));
    harness.register_model(model).await;

    let mut cfg = config(&harness);
    cfg.agent.limits.max_consecutive_tool_errors = 2;
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();

    assert_eq!(
        summary.stop,
        StopReason::EndTurn,
        "two failures separated by a success are not two *consecutive* failures"
    );
}

// --- cancellation ---------------------------------------------------------------------------

#[tokio::test]
async fn cancelling_mid_stream_ends_the_run() {
    let harness = Harness::new().await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_text("hello"))]));
    harness.register_model(model).await;

    let cfg = config(&harness);
    cfg.cancel.cancel();
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();
    assert_eq!(summary.stop, StopReason::Cancelled);
}

#[tokio::test]
async fn a_cooperative_tool_stops_and_is_recorded() {
    let harness = Harness::new().await;
    let cfg = config(&harness);
    // The tool trips the run token itself, so the interruption is guaranteed to land
    // inside the tool call. Sleeping in the test and hoping the run got that far is how a
    // cancellation test becomes a flaky one.
    harness
        .register_tool(Arc::new(SlowTool::tripping(cfg.cancel.clone())))
        .await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_tool_calls(&[(
        "slow",
        serde_json::json!({}),
    )]))]));
    harness.register_model(model).await;

    let started = std::time::Instant::now();
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();

    assert_eq!(summary.stop, StopReason::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the budget is five seconds"
    );
    let topics = harness.topics().await;
    assert!(
        topics.contains(&"tool.completed".to_string()),
        "a tool that stopped when asked is still a tool call that needs an answer: {topics:?}"
    );
}

#[tokio::test]
async fn a_tool_that_ignores_cancellation_is_abandoned_within_the_budget() {
    // Without a give-up path, "five seconds" is a hope rather than a promise.
    let harness = Harness::new().await;
    let cfg = config(&harness);
    harness
        .register_tool(Arc::new(StubbornTool::tripping(cfg.cancel.clone())))
        .await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_tool_calls(&[(
        "stubborn",
        serde_json::json!({}),
    )]))]));
    harness.register_model(model).await;

    let started = std::time::Instant::now();
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();

    assert_eq!(summary.stop, StopReason::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "budget exceeded"
    );

    let state = replay(&harness.events().await);
    let last = state.messages.last().unwrap();
    assert_eq!(last.role, rivet_core::model::Role::Tool);
    match &last.content[0] {
        rivet_core::model::ContentBlock::ToolResult { content, .. } => assert!(
            content.contains("Its effects, if any, are unknown"),
            "an abandoned tool may have done something: {content}"
        ),
        other => panic!("expected a tool result, got {other:?}"),
    }
}

// --- every call gets an answer, on every exit path -------------------------------------------

/// Assert that a log left by an interrupted run can actually be resumed.
async fn assert_resumable(harness: &Harness, expected_results: usize) {
    let state = replay(&harness.events().await);
    let tool_results = state
        .messages
        .iter()
        .filter(|m| m.role == rivet_core::model::Role::Tool)
        .count();
    assert_eq!(
        tool_results, expected_results,
        "every call in the abandoned turn needs an answer; got {tool_results}"
    );
    assert_eq!(
        session_recovery::inspect(&state.messages),
        Rli::Satisfied,
        "and they must be answered in place, not merely present"
    );

    // The real check: resume finds nothing left to repair.
    let repaired = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .expect("a correctly abandoned turn must leave a resumable log");
    assert_eq!(repaired.messages.len(), state.messages.len());
}

#[tokio::test]
async fn cancelling_inside_a_batch_still_answers_every_call() {
    // Doing cancellation right used to be the thing that broke `rivet resume`: the model
    // asked for three tools, one ran, and the other two were left unanswered forever.
    let harness = Harness::new().await;
    let cfg = config(&harness);
    harness
        .register_tool(Arc::new(SlowTool::tripping(cfg.cancel.clone())))
        .await;
    harness.register_tool(Arc::new(EchoTool)).await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_tool_calls(&[
        ("slow", serde_json::json!({})),
        ("echo", serde_json::json!({ "message": "b" })),
        ("echo", serde_json::json!({ "message": "c" })),
    ]))]));
    harness.register_model(model).await;

    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();
    assert_eq!(summary.stop, StopReason::Cancelled);
    assert_resumable(&harness, 3).await;

    let state = replay(&harness.events().await);
    let contents: Vec<String> = state
        .messages
        .iter()
        .filter(|m| m.role == rivet_core::model::Role::Tool)
        .filter_map(|m| match &m.content[0] {
            rivet_core::model::ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(
        contents[1].contains("never started"),
        "a call that never ran can be retried safely, and the wording says so: {contents:?}"
    );
}

#[tokio::test]
async fn a_deadline_inside_a_batch_still_answers_every_call() {
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(SlowTool::default())).await;
    harness.register_tool(Arc::new(EchoTool)).await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_tool_calls(&[
        ("slow", serde_json::json!({})),
        ("echo", serde_json::json!({ "message": "b" })),
    ]))]));
    harness.register_model(model).await;

    let mut cfg = config(&harness);
    cfg.agent.limits.max_duration_ms = 750;
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();

    assert_eq!(
        summary.stop,
        StopReason::LimitReached {
            limit: LimitKind::Duration
        }
    );
    assert_resumable(&harness, 2).await;
}

#[tokio::test]
async fn hitting_the_error_limit_inside_a_batch_still_answers_every_call() {
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(FailingTool)).await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_tool_calls(&[
        ("always_fails", serde_json::json!({})),
        ("always_fails", serde_json::json!({})),
        ("always_fails", serde_json::json!({})),
    ]))]));
    harness.register_model(model).await;

    let mut cfg = config(&harness);
    cfg.agent.limits.max_consecutive_tool_errors = 1;
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();

    assert_eq!(summary.stop, stop_reached_consecutive());
    assert_resumable(&harness, 3).await;
}

// --- retries and failures ---------------------------------------------------------------------

#[tokio::test]
async fn a_transient_failure_is_retried() {
    let harness = Harness::new().await;
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Fail(transient("connection reset")),
        Reply::Fail(transient("connection reset")),
        Reply::Sse(sse_text("third time lucky")),
    ]));
    harness.register_model(model.clone()).await;

    let summary = agent_loop(&harness)
        .run(config(&harness), harness.state().await, None)
        .await
        .unwrap();

    assert_eq!(summary.stop, StopReason::EndTurn);
    assert_eq!(
        summary.turns, 1,
        "a retry is not a turn: a turn is one request plus the tools it caused"
    );
    assert_eq!(model.request_count().await, 3);

    let requested = harness
        .topics()
        .await
        .iter()
        .filter(|t| *t == "model.requested")
        .count();
    assert_eq!(requested, 3, "each attempt is auditable");
}

#[tokio::test]
async fn a_permanent_failure_is_not_retried() {
    let harness = Harness::new().await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Fail(
        rivet_core::Error::invalid_argument("unknown parameter"),
    )]));
    harness.register_model(model.clone()).await;

    let summary = agent_loop(&harness)
        .run(config(&harness), harness.state().await, None)
        .await
        .unwrap();

    assert!(matches!(summary.stop, StopReason::Error { .. }));
    assert_eq!(model.request_count().await, 1);
}

#[tokio::test]
async fn an_output_limit_is_reported_as_an_incomplete_answer() {
    // Recording a truncated answer as a success is how a caller ends up acting on half a
    // plan. Continuation is a provider-dialect minefield and is a separate decision.
    let harness = Harness::new().await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_truncated(
        "here is the first half",
    ))]));
    harness.register_model(model).await;

    let summary = agent_loop(&harness)
        .run(config(&harness), harness.state().await, None)
        .await
        .unwrap();

    match &summary.stop {
        StopReason::Error { message } => assert!(message.contains("incomplete"), "{message}"),
        other => panic!("a truncated answer is not a success: {other:?}"),
    }
    assert!(!summary.stop.is_success());
    let state = replay(&harness.events().await);
    assert!(
        state.messages.last().unwrap().text().contains("first half"),
        "the partial answer is still recorded"
    );
}

#[tokio::test]
async fn a_log_that_cannot_be_written_stops_the_run() {
    // Continuing a run whose log is failing means the next resume reasons from a record
    // that stopped being true.
    let harness = Harness::with_store(|inner| Arc::new(FailingStore::new(inner, 2))).await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_text("hi"))]));
    harness.register_model(model).await;

    let error = agent_loop(&harness)
        .run(
            config(&harness),
            harness.state().await,
            Some(rivet_core::model::Message::user("go")),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), rivet_core::error::ErrorKind::Storage);
}

#[tokio::test]
async fn a_missing_model_is_reported_before_anything_is_written() {
    let harness = Harness::new().await;
    let before = harness.events().await.len();
    let error = agent_loop(&harness)
        .run(config(&harness), harness.state().await, None)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), rivet_core::error::ErrorKind::NotFound);
    assert_eq!(harness.events().await.len(), before, "nothing was written");
}
