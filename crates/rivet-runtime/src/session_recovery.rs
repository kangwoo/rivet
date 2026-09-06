//! Making an interrupted session resumable again.
//!
//! # The invariant
//!
//! In the message array a session log projects to, every assistant message carrying tool
//! calls is followed by a `ToolResult` for **each** of them, before the next assistant or
//! user message. Call it the resumable-log invariant. Every major provider rejects a
//! message array that breaks it with a 400, so a session that breaks it cannot be resumed
//! at all — not by this runtime, not by any other.
//!
//! Position matters, not just presence. This array answers every call and is still a 400:
//!
//! ```text
//! [user, assistant(tool_calls A,B), tool(A), assistant(text), tool(B)]
//!                                            ^^^^^^^^^^^^^^^  ^^^^^^^ too late
//! ```
//!
//! # Four layers hold it
//!
//! | | What | Where |
//! |---|---|---|
//! | M1 | The dispatcher writes exactly one terminating event per accepted call, cancellation included | [`crate::dispatch`] |
//! | M2 | The loop closes the calls of a turn it is leaving early, including ones never dispatched | [`crate::agent_loop`] |
//! | M3 | Resume closes whatever is still open, for the crash window M1 and M2 cannot cover | here |
//! | M4 | A log that appending cannot fix fails loudly instead of pretending | here |
//!
//! # Why the unresolved set comes from the projected messages
//!
//! [`SessionState::pending_tool_calls`] holds only calls that got a `tool.called` event,
//! and there are two ordinary ways to end a run without one:
//!
//! - the model asked for three tools, Ctrl-C landed after the first — the other two were
//!   never dispatched, so they are not pending, but they are unanswered;
//! - `kill -9` between writing the assistant message and writing `tool.called`.
//!
//! Both leave `pending_tool_calls` empty and the log unresumable. Reading the projected
//! messages instead catches every case, because that is the array the provider will see.
//!
//! # Why the synthetic results are *written*
//!
//! [`SessionState::replay`] already closes a dangling call in memory, but only correctly
//! when the log ends there. Resume, then one more turn, then replay again gives:
//!
//! ```text
//! [user, assistant(tool_calls A), assistant(text), tool_result(A)]
//! ```
//!
//! — an orphaned tool message at the end, which is the 400 this module exists to prevent.
//! So the correction is appended to the log, exactly once: `apply` removes the call from
//! the pending set when it folds the result, so a second resume finds nothing to do.

use std::collections::{HashMap, HashSet};

use rivet_core::error::Error;
use rivet_core::id::{RunId, SessionId, ToolCallId};
use rivet_core::model::{ContentBlock, Message, Role};
use rivet_core::session::{Expect, SessionEvent, SessionState, SessionStore, StoredEvent};
use rivet_core::tool::ToolResult;

/// Result text for a call that started and whose effects are unknown.
///
/// Byte-for-byte the string [`SessionState::close_interrupted_calls`] uses. `rivet-core`
/// does not expose it as a constant and adding one would be a contract change, so it is
/// duplicated here and `tests::the_synthetic_close_matches_core` fails if either drifts.
pub const INTERRUPTED_TOOL_RESULT: &str = "The runtime stopped before this tool finished. \
     Its effects, if any, are unknown; verify before retrying.";

/// Result text for a call that provably never ran.
///
/// The distinction matters to the model: this one is safe to retry, the other is not.
pub const NOT_STARTED_TOOL_RESULT: &str =
    "This tool was never started; the run stopped first. It had no effect.";

/// How many events to pull per read while scanning a log.
const READ_PAGE: usize = 10_000;

/// What the projected message array says about the invariant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rli {
    /// Every tool call is answered in place.
    Satisfied,
    /// The final assistant message has unanswered calls and only its own results follow.
    /// Appending the missing results restores the invariant.
    Repairable(Vec<ToolCallId>),
    /// Appending cannot fix it: a log may only be written at its end.
    Broken(String),
}

/// Inspect a projected message array.
///
/// This is the whole-array, position-aware check. An existence check ("is every call id
/// answered somewhere?") passes the example in the module documentation, which is exactly
/// the array it is supposed to reject.
#[must_use]
pub fn inspect(messages: &[Message]) -> Rli {
    let mut index = 0usize;
    while index < messages.len() {
        let message = &messages[index];

        if message.role == Role::Tool {
            return Rli::Broken(format!(
                "the tool result at position {index} answers no preceding tool call"
            ));
        }

        let requested: Vec<ToolCallId> = message.tool_calls().iter().map(|c| c.id).collect();
        if message.role != Role::Assistant || requested.is_empty() {
            index += 1;
            continue;
        }

        // Every answer has to arrive in the run of tool messages that follows.
        let mut answered: HashSet<ToolCallId> = HashSet::new();
        let mut cursor = index + 1;
        while cursor < messages.len() && messages[cursor].role == Role::Tool {
            for block in &messages[cursor].content {
                if let ContentBlock::ToolResult { call_id, .. } = block {
                    if !requested.contains(call_id) {
                        return Rli::Broken(format!(
                            "the tool result at position {cursor} answers a call \
                             that is not in the assistant message at position {index}"
                        ));
                    }
                    answered.insert(*call_id);
                }
            }
            cursor += 1;
        }

        let missing: Vec<ToolCallId> = requested
            .into_iter()
            .filter(|id| !answered.contains(id))
            .collect();
        if !missing.is_empty() {
            return if cursor == messages.len() {
                // The log ends here, so appending puts the results in the right place.
                Rli::Repairable(missing)
            } else {
                Rli::Broken(format!(
                    "{} tool call(s) in the assistant message at position {index} are \
                     unanswered, but the log continues at position {cursor}; a result \
                     appended now would land in the wrong place",
                    missing.len()
                ))
            };
        }
        index = cursor;
    }
    Rli::Satisfied
}

/// Close every unresolved tool call in a session, durably.
///
/// Idempotent: a session with nothing open is read and left alone.
///
/// # Errors
/// - Whatever the store returns.
/// - [`rivet_core::error::ErrorKind::Storage`] when the log cannot be repaired by
///   appending. The message names the recovery path (`rivet session fork`), because
///   pretending to fix it would produce a session that fails on its next request instead.
pub async fn close_interrupted(
    store: &dyn SessionStore,
    id: SessionId,
) -> rivet_core::Result<SessionState> {
    let events = read_all(store, id).await?;

    // `apply`, not `replay`: this must see the projection *before* the in-memory tail
    // correction, or every session would look already closed.
    let mut state = SessionState::default();
    for stored in &events {
        state.apply(stored);
    }

    let unresolved = match inspect(&state.messages) {
        Rli::Satisfied => return Ok(SessionState::replay(&events)),
        Rli::Repairable(ids) => ids,
        Rli::Broken(reason) => {
            return Err(Error::storage(format!(
                "session `{id}` cannot be resumed: {reason}. \
                 Branch before the problem and continue from there: \
                 `rivet session fork {id} <seq>`."
            )));
        }
    };

    let origins = call_origins(&events);
    let mut last_seq = state.last_seq;
    for call_id in unresolved {
        let origin = origins.get(&call_id).copied().unwrap_or_default();
        let content = if origin.dispatched {
            INTERRUPTED_TOOL_RESULT
        } else {
            // Provably no effect: the pipeline writes `tool.called` before it executes.
            NOT_STARTED_TOOL_RESULT
        };
        let stored = store
            .append(
                id,
                Expect::Seq(last_seq),
                SessionEvent::ToolCompleted {
                    // The run that *made* the call, not the one resuming: a call and its
                    // result belong to the same run or the audit trail stops making sense.
                    run_id: origin.run_id.unwrap_or_else(RunId::new),
                    call_id,
                    result: ToolResult::error(content),
                    duration_ms: 0,
                },
            )
            .await?;
        // Advance, or the second close fails its own `Expect::Seq`.
        last_seq = stored.seq;
    }

    Ok(SessionState::replay(&read_all(store, id).await?))
}

/// What is known about where a call came from.
#[derive(Clone, Copy, Debug, Default)]
struct Origin {
    run_id: Option<RunId>,
    /// Whether a `tool.called` event exists, i.e. whether the tool could have run.
    dispatched: bool,
}

fn call_origins(events: &[StoredEvent]) -> HashMap<ToolCallId, Origin> {
    let mut origins: HashMap<ToolCallId, Origin> = HashMap::new();
    for stored in events {
        match &stored.event {
            SessionEvent::AssistantMessage {
                run_id, message, ..
            } => {
                for call in message.tool_calls() {
                    let entry = origins.entry(call.id).or_default();
                    entry.run_id.get_or_insert(*run_id);
                }
            }
            SessionEvent::ToolCalled { run_id, call } => {
                let entry = origins.entry(call.id).or_default();
                entry.run_id = Some(*run_id);
                entry.dispatched = true;
            }
            _ => {}
        }
    }
    origins
}

/// Read a whole log, a page at a time.
pub async fn read_all(
    store: &dyn SessionStore,
    id: SessionId,
) -> rivet_core::Result<Vec<StoredEvent>> {
    let mut all = Vec::new();
    let mut from = 1u64;
    loop {
        let page = store.read(id, from, READ_PAGE).await?;
        let fetched = page.len();
        if let Some(last) = page.last() {
            from = last.seq + 1;
        }
        all.extend(page);
        if fetched < READ_PAGE {
            return Ok(all);
        }
    }
}

/// What `rivet resume` should do with a session, once its log is repaired.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumePlan {
    /// Run a turn with the conversation as it stands.
    Continue,
    /// There is nothing a turn could add.
    Refuse(String),
}

/// Decide whether a repaired session has anywhere to go.
///
/// `rivet resume <session>` takes no new prompt, so a session that already ended cleanly
/// would just re-ask the same question and spend tokens on the same answer.
#[must_use]
pub fn resume_plan(state: &SessionState) -> ResumePlan {
    if state.closed {
        return ResumePlan::Refuse(
            "this session is closed. Start a new one with `rivet \"…\"`, \
             or branch it with `rivet session fork`."
                .to_string(),
        );
    }

    let Some(last) = state.messages.last() else {
        return ResumePlan::Refuse(
            "this session has no conversation to resume. Start one with `rivet \"…\"`.".to_string(),
        );
    };

    match last.role {
        // Three resumable shapes, and the first is the *most common* one: a tool result
        // was recorded and the run stopped before asking the model what to do with it, so
        // the model has never seen it. A condition table that omits this row blocks the
        // main path of `rivet resume`. The others: the model never got to answer, or an
        // unanswered call survived `close_interrupted` (only possible when the caller
        // skipped it).
        Role::Tool | Role::User | Role::System => ResumePlan::Continue,
        Role::Assistant if !last.tool_calls().is_empty() => ResumePlan::Continue,
        Role::Assistant => ResumePlan::Refuse(
            "this session already finished its turn. Ask something new with \
             `rivet \"…\"`, or branch it with `rivet session fork`."
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::tool::ToolCall;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: name.into(),
            input: serde_json::json!({}),
        }
    }

    fn assistant(calls: &[&ToolCall]) -> Message {
        Message {
            role: Role::Assistant,
            content: calls
                .iter()
                .map(|c| ContentBlock::ToolCall((*c).clone()))
                .collect(),
        }
    }

    fn result(call: &ToolCall) -> Message {
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id: call.id,
                content: "ok".into(),
                is_error: false,
            }],
        }
    }

    #[test]
    fn the_synthetic_close_matches_core_byte_for_byte() {
        // If this fails, a resumed session and a replayed one disagree about what the
        // model was told, and the drift is invisible until a provider rejects the array.
        let call_id = ToolCallId::new();
        let mut state = SessionState::default();
        state.apply(&StoredEvent {
            seq: 1,
            at: rivet_core::Timestamp::now(),
            event: SessionEvent::ToolCalled {
                run_id: RunId::new(),
                call: ToolCall {
                    id: call_id,
                    name: "read_file".into(),
                    input: serde_json::json!({}),
                },
            },
        });
        state.close_interrupted_calls();
        match &state.messages[0].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(content, INTERRUPTED_TOOL_RESULT);
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[test]
    fn an_answered_conversation_satisfies_the_invariant() {
        let a = call("read_file");
        let messages = vec![
            Message::user("read it"),
            assistant(&[&a]),
            result(&a),
            Message::assistant("here it is"),
        ];
        assert_eq!(inspect(&messages), Rli::Satisfied);
    }

    #[test]
    fn a_log_ending_at_an_unanswered_call_is_repairable() {
        let a = call("read_file");
        let messages = vec![Message::user("read it"), assistant(&[&a])];
        assert_eq!(inspect(&messages), Rli::Repairable(vec![a.id]));
    }

    #[test]
    fn a_partially_dispatched_batch_is_repairable() {
        // Ctrl-C after the first of three: B and C were never dispatched, so nothing is
        // "pending", but the array is unanswerable as it stands.
        let (a, b, c) = (call("read_file"), call("list_dir"), call("search"));
        let messages = vec![
            Message::user("do three things"),
            assistant(&[&a, &b, &c]),
            result(&a),
        ];
        assert_eq!(inspect(&messages), Rli::Repairable(vec![b.id, c.id]));
    }

    #[test]
    fn an_answer_that_arrives_after_another_message_is_not_repairable() {
        // The array this module exists to reject: every call is answered, and it is still
        // a 400, because appending can only add to the end.
        let (a, b) = (call("read_file"), call("list_dir"));
        let messages = vec![
            Message::user("two things"),
            assistant(&[&a, &b]),
            result(&a),
            Message::assistant("thinking out loud"),
            result(&b),
        ];
        match inspect(&messages) {
            Rli::Broken(reason) => assert!(reason.contains("wrong place"), "{reason}"),
            other => panic!("an existence check would call this fine: {other:?}"),
        }
    }

    #[test]
    fn a_second_call_group_before_the_first_is_answered_is_not_repairable() {
        let (a, b) = (call("read_file"), call("list_dir"));
        let messages = vec![
            Message::user("go"),
            assistant(&[&a]),
            assistant(&[&b]),
            result(&a),
        ];
        assert!(matches!(inspect(&messages), Rli::Broken(_)));
    }

    #[test]
    fn an_orphaned_tool_result_is_not_repairable() {
        let a = call("read_file");
        let messages = vec![Message::user("go"), result(&a)];
        assert!(matches!(inspect(&messages), Rli::Broken(_)));
    }

    #[test]
    fn resume_continues_when_the_last_message_is_a_tool_result() {
        // The most common resumable state, and the one a naive condition table omits: the
        // result was recorded and the run stopped before the model saw it.
        let a = call("read_file");
        let mut state = SessionState::default();
        state.messages = vec![Message::user("read it"), assistant(&[&a]), result(&a)];
        assert_eq!(resume_plan(&state), ResumePlan::Continue);
    }

    #[test]
    fn resume_continues_when_the_model_never_answered() {
        let mut state = SessionState::default();
        state.messages = vec![Message::user("explain this repo")];
        assert_eq!(resume_plan(&state), ResumePlan::Continue);
    }

    #[test]
    fn resume_refuses_a_finished_conversation() {
        let mut state = SessionState::default();
        state.messages = vec![Message::user("hi"), Message::assistant("hello")];
        match resume_plan(&state) {
            ResumePlan::Refuse(reason) => assert!(reason.contains("already finished"), "{reason}"),
            other @ ResumePlan::Continue => {
                panic!("re-asking the same question spends tokens for nothing: {other:?}")
            }
        }
    }

    #[test]
    fn resume_refuses_a_closed_session() {
        let mut state = SessionState::default();
        state.messages = vec![Message::user("hi")];
        state.closed = true;
        assert!(matches!(resume_plan(&state), ResumePlan::Refuse(_)));
    }

    #[test]
    fn resume_refuses_an_empty_session() {
        assert!(matches!(
            resume_plan(&SessionState::default()),
            ResumePlan::Refuse(_)
        ));
    }
}
