//! Resuming an interrupted session, against a real log on disk.
//!
//! The shapes here are the ones a `kill -9` and a Ctrl-C actually leave behind, and each
//! is checked the way the provider will check it: does the message array come out
//! well-formed, with every tool call answered *in place*?

mod support;

use std::sync::Arc;

use rivet_core::agent::StopReason;
use rivet_core::id::{RunId, ToolCallId};
use rivet_core::model::{ContentBlock, Message, Role, StopReason as ModelStop, Usage};
use rivet_core::retry::ExponentialBackoff;
use rivet_core::session::{Expect, SessionEvent};
use rivet_core::tool::{ToolCall, ToolResult};
use rivet_runtime::agent_loop::{AgentLoop, RunConfig};
use rivet_runtime::jitter::NoJitter;
use rivet_runtime::session_recovery::{
    self, INTERRUPTED_TOOL_RESULT, NOT_STARTED_TOOL_RESULT, ResumePlan, Rli,
};
use support::{EchoTool, FixtureModel, Harness, Reply, replay, sse_text, sse_tool_calls};

fn call(name: &str) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        name: name.into(),
        input: serde_json::json!({ "message": "hi" }),
    }
}

fn assistant_calling(run_id: RunId, calls: &[&ToolCall]) -> SessionEvent {
    SessionEvent::AssistantMessage {
        run_id,
        message: Message {
            role: Role::Assistant,
            content: calls
                .iter()
                .map(|c| ContentBlock::ToolCall((*c).clone()))
                .collect(),
        },
        stop_reason: ModelStop::ToolUse,
        usage: Usage::default(),
    }
}

/// Append a sequence of events to the harness session, in order.
async fn seed(harness: &Harness, events: Vec<SessionEvent>) {
    let mut seq = harness.store.last_seq(harness.session_id).await.unwrap();
    for event in events {
        seq = harness
            .store
            .append(harness.session_id, Expect::Seq(seq), event)
            .await
            .unwrap()
            .seq;
    }
}

/// The tool result texts a replay produces, in order.
fn tool_texts(state: &rivet_core::session::SessionState) -> Vec<String> {
    state
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| match &m.content[0] {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_crash_after_tool_called_is_closed_as_unknown() {
    // The tool started, so its effects cannot be ruled out.
    let harness = Harness::new().await;
    let run_id = RunId::new();
    let a = call("echo");
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("echo hi"),
            },
            assistant_calling(run_id, &[&a]),
            SessionEvent::ToolCalled {
                run_id,
                call: a.clone(),
            },
        ],
    )
    .await;

    let state = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap();

    assert_eq!(tool_texts(&state), [INTERRUPTED_TOOL_RESULT]);
    assert_eq!(session_recovery::inspect(&state.messages), Rli::Satisfied);
}

#[tokio::test]
async fn a_crash_before_tool_called_is_closed_as_never_started() {
    // `pending_tool_calls` is empty here -- the very case a `pending`-based recovery
    // misses -- and the pipeline proves the tool had no chance to run, so the model can
    // retry it safely.
    let harness = Harness::new().await;
    let run_id = RunId::new();
    let a = call("echo");
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("echo hi"),
            },
            assistant_calling(run_id, &[&a]),
        ],
    )
    .await;

    let before = replay(&harness.events().await);
    assert!(
        before.pending_tool_calls().is_empty(),
        "nothing is pending, and yet the log is unresumable"
    );

    let state = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap();
    assert_eq!(tool_texts(&state), [NOT_STARTED_TOOL_RESULT]);
}

#[tokio::test]
async fn a_partly_dispatched_batch_is_closed_with_the_right_wording_for_each_call() {
    let harness = Harness::new().await;
    let run_id = RunId::new();
    let (a, b, c) = (call("echo"), call("echo"), call("echo"));
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("three things"),
            },
            assistant_calling(run_id, &[&a, &b, &c]),
            SessionEvent::ToolCalled {
                run_id,
                call: a.clone(),
            },
            SessionEvent::ToolCompleted {
                run_id,
                call_id: a.id,
                result: ToolResult::ok("first"),
                duration_ms: 1,
            },
            // B was dispatched and never finished; C never started at all.
            SessionEvent::ToolCalled {
                run_id,
                call: b.clone(),
            },
        ],
    )
    .await;

    let state = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap();

    assert_eq!(
        tool_texts(&state),
        ["first", INTERRUPTED_TOOL_RESULT, NOT_STARTED_TOOL_RESULT],
        "the distinction is what tells the model which calls are safe to retry"
    );
    assert_eq!(session_recovery::inspect(&state.messages), Rli::Satisfied);
}

#[tokio::test]
async fn closing_is_idempotent() {
    let harness = Harness::new().await;
    let run_id = RunId::new();
    let a = call("echo");
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("go"),
            },
            assistant_calling(run_id, &[&a]),
        ],
    )
    .await;

    session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap();
    let after_first = harness.events().await.len();
    session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap();
    assert_eq!(
        harness.events().await.len(),
        after_first,
        "a second resume has nothing left to close"
    );
}

#[tokio::test]
async fn closing_several_calls_advances_the_expected_sequence_number() {
    // Reusing one `last_seq` for every append makes the second one fail with
    // InvalidArgument, and a session with two open calls would be unrecoverable.
    let harness = Harness::new().await;
    let run_id = RunId::new();
    let (a, b, c) = (call("echo"), call("echo"), call("echo"));
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("go"),
            },
            assistant_calling(run_id, &[&a, &b, &c]),
        ],
    )
    .await;

    let state = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .expect("three open calls must all close");
    assert_eq!(tool_texts(&state).len(), 3);
}

#[tokio::test]
async fn a_log_that_appending_cannot_fix_fails_loudly() {
    // Every call is answered and the array is still a 400, because B's answer is in the
    // wrong place. There is no way to add to the end that fixes it, so pretending to fix
    // it would just move the failure to the next request.
    let harness = Harness::new().await;
    let run_id = RunId::new();
    let (a, b) = (call("echo"), call("echo"));
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("two things"),
            },
            assistant_calling(run_id, &[&a, &b]),
            SessionEvent::ToolCompleted {
                run_id,
                call_id: a.id,
                result: ToolResult::ok("first"),
                duration_ms: 1,
            },
            SessionEvent::AssistantMessage {
                run_id,
                message: Message::assistant("thinking out loud"),
                stop_reason: ModelStop::EndTurn,
                usage: Usage::default(),
            },
            SessionEvent::ToolCompleted {
                run_id,
                call_id: b.id,
                result: ToolResult::ok("too late"),
                duration_ms: 1,
            },
        ],
    )
    .await;

    let error = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), rivet_core::error::ErrorKind::Storage);
    assert!(
        error.message().contains("session fork"),
        "the error has to name a way out: {error}"
    );
}

#[tokio::test]
async fn the_synthetic_close_agrees_with_an_in_memory_replay() {
    // `SessionState::replay` closes a dangling call in memory; this module writes the
    // same close to the log. If the two ever disagree, a resumed session and a replayed
    // one would show the model different text -- and the drift would be invisible until a
    // provider rejected it.
    //
    // The equivalence only holds for a log where `tool.called` was written: without it,
    // `replay` sees nothing pending and this module (correctly) says "never started".
    let harness = Harness::new().await;
    let run_id = RunId::new();
    let a = call("echo");
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("go"),
            },
            assistant_calling(run_id, &[&a]),
            SessionEvent::ToolCalled {
                run_id,
                call: a.clone(),
            },
        ],
    )
    .await;

    let in_memory = replay(&harness.events().await);
    let persisted = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap();
    pretty_assertions::assert_eq!(in_memory.messages, persisted.messages);
}

// --- resume, all the way through a turn ------------------------------------------------------

fn agent_loop(harness: &Harness) -> AgentLoop {
    AgentLoop::new(
        harness.registry.clone(),
        harness.store.clone(),
        harness.bus.clone(),
        harness.assembler(),
        Arc::new(ExponentialBackoff::default()),
        Arc::new(NoJitter),
    )
}

#[tokio::test]
async fn resume_replays_the_conversation_into_the_next_request() {
    // DoD 4: the resumed run's request carries what was said before, in order.
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(EchoTool)).await;
    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_text("continuing"))]));
    harness.register_model(model.clone()).await;

    let run_id = RunId::new();
    let a = call("echo");
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("echo hi"),
            },
            assistant_calling(run_id, &[&a]),
            SessionEvent::ToolCalled {
                run_id,
                call: a.clone(),
            },
        ],
    )
    .await;

    let state = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap();
    assert_eq!(session_recovery::resume_plan(&state), ResumePlan::Continue);

    let summary = agent_loop(&harness)
        .run(
            RunConfig::new(
                harness.agent(),
                harness.session_id,
                harness.workspace.clone(),
            ),
            state,
            None,
        )
        .await
        .unwrap();
    assert_eq!(summary.stop, StopReason::EndTurn);

    let sent = &model.requests().await[0];
    let texts: Vec<String> = sent.messages.iter().map(Message::text).collect();
    assert_eq!(
        texts[0], "echo hi",
        "the earlier turn is replayed, in order"
    );
    assert_eq!(
        sent.messages.last().unwrap().role,
        Role::Tool,
        "and the synthetic result is the last thing the model sees"
    );
}

#[tokio::test]
async fn resume_then_a_turn_then_resume_leaves_no_orphan_tool_message() {
    // The regression that made the synthetic close a *written* event rather than an
    // in-memory one: `replay`'s tail correction is only right when the log ends there. Two
    // resumes in a row would not catch this -- a turn has to happen in between.
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(EchoTool)).await;
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_text("after the interruption")),
        Reply::Sse(sse_text("and again")),
    ]));
    harness.register_model(model.clone()).await;

    let run_id = RunId::new();
    let a = call("echo");
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("go"),
            },
            assistant_calling(run_id, &[&a]),
            SessionEvent::ToolCalled {
                run_id,
                call: a.clone(),
            },
        ],
    )
    .await;

    // Resume once, run a turn.
    let state = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap();
    agent_loop(&harness)
        .run(
            RunConfig::new(
                harness.agent(),
                harness.session_id,
                harness.workspace.clone(),
            ),
            state,
            None,
        )
        .await
        .unwrap();

    // Now replay from scratch, the way a fresh process would.
    let state = replay(&harness.events().await);
    assert_eq!(
        session_recovery::inspect(&state.messages),
        Rli::Satisfied,
        "an orphaned tool message at the end is the 400 this design exists to prevent"
    );
    assert_eq!(
        state.messages.last().unwrap().role,
        Role::Assistant,
        "the conversation ends with the answer, not with a stray result"
    );
}

#[tokio::test]
async fn a_cancelled_run_can_be_resumed_and_finished() {
    // The full loop: interrupt a run mid-batch, resume it, and let it finish. This is
    // where DoD 3 and DoD 4 meet -- doing cancellation right used to break resuming.
    let harness = Harness::new().await;
    let mut cfg = RunConfig::new(
        harness.agent(),
        harness.session_id,
        harness.workspace.clone(),
    );
    cfg.cancel_grace = std::time::Duration::from_millis(200);
    // The tool trips the run token itself, so the interruption always lands inside the
    // batch rather than racing the round trip before it.
    harness
        .register_tool(Arc::new(support::SlowTool::tripping(cfg.cancel.clone())))
        .await;
    harness.register_tool(Arc::new(EchoTool)).await;
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_tool_calls(&[
            ("slow", serde_json::json!({})),
            ("echo", serde_json::json!({ "message": "b" })),
        ])),
        Reply::Sse(sse_text("finished after the interruption")),
    ]));
    harness.register_model(model.clone()).await;

    let interrupted = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .unwrap();
    assert_eq!(interrupted.stop, StopReason::Cancelled);

    let state = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .expect("a cancelled run must leave a resumable log");
    assert_eq!(session_recovery::resume_plan(&state), ResumePlan::Continue);

    let resumed = agent_loop(&harness)
        .run(
            RunConfig::new(
                harness.agent(),
                harness.session_id,
                harness.workspace.clone(),
            ),
            state,
            None,
        )
        .await
        .unwrap();
    assert_eq!(resumed.stop, StopReason::EndTurn);

    let sent = model.requests().await;
    let last = sent.last().unwrap();
    let results = support::tool_results_in(last);
    assert_eq!(
        results.len(),
        2,
        "both calls from the interrupted turn are answered: {results:?}"
    );
}

#[tokio::test]
async fn a_finished_session_is_not_resumed() {
    // `rivet resume` takes no new prompt, so re-running a finished conversation spends
    // tokens to produce the same answer.
    let harness = Harness::new().await;
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("hi"),
            },
            SessionEvent::AssistantMessage {
                run_id: RunId::new(),
                message: Message::assistant("hello"),
                stop_reason: ModelStop::EndTurn,
                usage: Usage::default(),
            },
        ],
    )
    .await;

    let state = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap();
    match session_recovery::resume_plan(&state) {
        ResumePlan::Refuse(reason) => assert!(reason.contains("already finished"), "{reason}"),
        ResumePlan::Continue => panic!("there is nothing for another turn to add"),
    }
}

#[tokio::test]
async fn a_session_whose_last_message_is_a_tool_result_is_resumed() {
    // The most common resumable state of all, and the one a condition table taken
    // literally would leave with no matching row.
    let harness = Harness::new().await;
    let run_id = RunId::new();
    let a = call("echo");
    seed(
        &harness,
        vec![
            SessionEvent::UserMessage {
                message: Message::user("go"),
            },
            assistant_calling(run_id, &[&a]),
            SessionEvent::ToolCalled {
                run_id,
                call: a.clone(),
            },
            SessionEvent::ToolCompleted {
                run_id,
                call_id: a.id,
                result: ToolResult::ok("hi"),
                duration_ms: 1,
            },
        ],
    )
    .await;

    let state = session_recovery::close_interrupted(harness.store.as_ref(), harness.session_id)
        .await
        .unwrap();
    assert_eq!(
        session_recovery::resume_plan(&state),
        ResumePlan::Continue,
        "the model has not seen this result yet; a turn is exactly what is missing"
    );
}
