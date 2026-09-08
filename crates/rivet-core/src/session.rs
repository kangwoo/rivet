//! The session contract: an append-only log that is the source of truth.
//!
//! The distinction that matters: **session events are durable facts, bus events are
//! notifications.** `agent.text.delta` goes on the bus and is never persisted;
//! `assistant.message` is persisted and never needs re-derivation from deltas. Conflating
//! them produces a log you cannot replay deterministically.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::id::{AgentId, JobId, RunId, SessionId, ToolCallId};
use crate::model::{Message, ModelId, StopReason, Usage};
use crate::time::Timestamp;
use crate::tool::{ToolCall, ToolResult};

/// A durable fact.
///
/// Every variant must be replayable: applying the log from the start reproduces the exact
/// state. Nothing here may reference a value that only existed in memory.
///
/// The wire tag of every variant is pinned explicitly rather than derived from the
/// Rust name. This is an on-disk format: renaming a variant in Rust must never
/// silently invalidate every session log a user already has. `tests::wire_names_are_pinned`
/// fails if one drifts.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionEvent {
    #[serde(rename = "session.created")]
    Created {
        /// Workspace root at creation time, recorded so a replay can detect that it is
        /// being applied somewhere else.
        workspace_root: String,
        parent: Option<ForkPoint>,
    },
    #[serde(rename = "run.started")]
    RunStarted {
        run_id: RunId,
        agent_id: AgentId,
        model: ModelId,
        job_id: Option<JobId>,
    },
    #[serde(rename = "user.message")]
    UserMessage { message: Message },
    /// Recorded *before* the request goes out, so a crash mid-request is visible on
    /// replay as a request with no completion.
    #[serde(rename = "model.requested")]
    ModelRequested {
        run_id: RunId,
        model: ModelId,
        /// Hash of the assembled request, for cache analysis and reproducibility checks.
        request_digest: String,
    },
    #[serde(rename = "assistant.message")]
    AssistantMessage {
        run_id: RunId,
        message: Message,
        stop_reason: StopReason,
        usage: Usage,
    },
    #[serde(rename = "tool.called")]
    ToolCalled { run_id: RunId, call: ToolCall },
    #[serde(rename = "tool.completed")]
    ToolCompleted {
        run_id: RunId,
        call_id: ToolCallId,
        result: ToolResult,
        duration_ms: u64,
    },
    /// A tool never ran. Persisted because "the model asked and was refused" is exactly
    /// the fact an audit needs.
    #[serde(rename = "tool.blocked")]
    ToolBlocked {
        run_id: RunId,
        call_id: ToolCallId,
        policy: String,
        reason: String,
    },
    /// This call went through an approval decision. Durable because "who approved
    /// `rm -rf`, and when" is precisely the question an audit asks, and the bus is lossy.
    ///
    /// Not "a human was asked": the pair is written on every path through step 6,
    /// including the ones nobody was asked on. Who answered is
    /// [`SessionEvent::ApprovalResolved::actor`], and `None` there means a rule answered
    /// rather than a person — an unattended run, a remembered grant, a cancellation.
    /// Skipping the pair when nothing was asked would leave "the model asked for
    /// something approvable and was refused" recoverable only by parsing the reason
    /// string on `tool.blocked`, which is the one fact the audit came for.
    #[serde(rename = "approval.requested")]
    ApprovalRequested {
        run_id: RunId,
        call_id: ToolCallId,
        reason: String,
        preview: String,
        /// Stable identity of *what* is being approved, supplied by the policy.
        ///
        /// Not the call id: every later call gets a fresh one, so a grant keyed on it
        /// could never match anything and "allow for this session" would silently do
        /// nothing. A policy builds this from the tool name plus whatever it actually
        /// cares about (`shell:git-push`, `write_file:src/**`).
        scope_key: String,
    },
    /// The human's answer.
    ///
    /// `remembered` records an "allow for the rest of this session" grant. Keeping it in
    /// the log rather than in memory is what makes a resumed session behave like the
    /// original: remembered approvals are a projection of this event, not runtime state
    /// that evaporates on restart.
    #[serde(rename = "approval.resolved")]
    ApprovalResolved {
        run_id: RunId,
        call_id: ToolCallId,
        /// Matches the `scope_key` of the corresponding request.
        scope_key: String,
        outcome: crate::policy::ApprovalOutcome,
        remembered: bool,
        /// Who answered. `None` for an automatic resolution (timeout, unattended).
        actor: Option<String>,
    },
    #[serde(rename = "run.completed")]
    RunCompleted {
        run_id: RunId,
        stop: crate::agent::StopReason,
        turns: u32,
    },
    /// A compaction boundary. Everything before it is summarized by `summary`; a replay
    /// may start here instead of at the beginning.
    #[serde(rename = "session.checkpoint")]
    Checkpoint {
        summary: String,
        /// Sequence number of the last event folded into the summary.
        through_seq: u64,
    },
    #[serde(rename = "session.closed")]
    Closed { reason: String },
}

/// Where a forked session branched from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkPoint {
    pub session_id: SessionId,
    /// Sequence number of the last inherited event.
    pub at_seq: u64,
}

/// A stored event: the fact plus its position and time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredEvent {
    /// Dense, gap-free, starting at 1. Used for optimistic concurrency on append.
    pub seq: u64,
    pub at: Timestamp,
    pub event: SessionEvent,
}

/// Summary row for listing sessions without reading their logs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub last_seq: u64,
    /// First user message, truncated. For `rivet session list`.
    pub title: String,
    pub closed: bool,
}

/// What a writer believes the current sequence number to be.
///
/// Spelled as an enum rather than `Option<u64>` so that giving up concurrency protection
/// is a word the author had to type. `Any` is for a fresh session or a repair tool, never
/// for the agent loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    /// Append only if `last_seq == n`.
    Seq(u64),
    /// Append unconditionally.
    Any,
}

/// Persistence for session logs.
///
/// # Durability
///
/// The whole design rests on the log surviving a crash — that is what `resume`, audit and
/// replay mean. So [`SessionStore::append`] must not return until the event is durable on
/// the underlying medium (`fsync`, or the backing store's equivalent). An implementation
/// that buffers and returns early loses the tail of the log in exactly the crash this
/// system exists to recover from.
///
/// Batching is allowed; returning before the batch is durable is not.
///
/// # Concurrency
///
/// The [`Expect`] parameter is what makes concurrent writers safe: two runs appending to
/// one session is a bug, and this surfaces it as a conflict rather than an interleaved
/// log.
#[async_trait]
pub trait SessionStore: Send + Sync + fmt::Debug {
    async fn create(&self, id: SessionId, event: SessionEvent) -> crate::Result<StoredEvent>;

    /// Append one event, returning only once it is durable.
    ///
    /// On an [`Expect::Seq`] mismatch, return
    /// [`crate::error::ErrorKind::InvalidArgument`] — do not silently accept.
    async fn append(
        &self,
        id: SessionId,
        expect: Expect,
        event: SessionEvent,
    ) -> crate::Result<StoredEvent>;

    /// Read events in `[from_seq, from_seq + limit)`, ordered by `seq`.
    async fn read(
        &self,
        id: SessionId,
        from_seq: u64,
        limit: usize,
    ) -> crate::Result<Vec<StoredEvent>>;

    async fn last_seq(&self, id: SessionId) -> crate::Result<u64>;

    async fn list(&self, limit: usize) -> crate::Result<Vec<SessionSummary>>;

    /// Create a new session inheriting `[1, at_seq]` from `source`.
    async fn fork(
        &self,
        source: SessionId,
        at_seq: u64,
        new_id: SessionId,
    ) -> crate::Result<SessionSummary>;
}

/// The in-memory projection built by folding a log.
#[derive(Clone, Debug, Default)]
pub struct SessionState {
    pub messages: Vec<Message>,
    pub last_seq: u64,
    pub total_usage: Usage,
    pub closed: bool,
    /// Set when a checkpoint has folded earlier history away.
    pub checkpoint_summary: Option<String>,
    /// Tool calls that were dispatched but never resolved. Non-empty means the log ends
    /// mid-turn — see [`SessionState::replay`].
    pending_tool_calls: Vec<ToolCallId>,
    /// Approvals the user asked to be remembered for the rest of the session.
    remembered_approvals: Vec<String>,
}

impl SessionState {
    /// Fold events into state. Pure: same input, same output, no clock, no I/O.
    ///
    /// # Resuming mid-turn
    ///
    /// A crash between `tool.called` and `tool.completed` leaves an assistant message
    /// holding a tool call that nothing ever answered. Every major provider rejects such
    /// a message list with a 400, so a naive replay produces a session that cannot be
    /// resumed at all.
    ///
    /// Replay therefore closes the gap: any call still outstanding when the log ends gets
    /// a synthetic error result. The model sees that the tool was interrupted and can
    /// decide whether to retry, which is exactly what a human would do.
    #[must_use]
    pub fn replay(events: &[StoredEvent]) -> Self {
        let mut state = Self::default();
        for stored in events {
            state.apply(stored);
        }
        state.close_interrupted_calls();
        state
    }

    /// Whether `scope_key` was already approved for the rest of this session.
    ///
    /// The policy layer calls this before asking again, which is what makes "don't ask me
    /// again" survive a resume.
    #[must_use]
    pub fn is_pre_approved(&self, scope_key: &str) -> bool {
        self.remembered_approvals.iter().any(|k| k == scope_key)
    }

    /// Calls dispatched but never resolved at the point replay stopped.
    #[must_use]
    pub fn pending_tool_calls(&self) -> &[ToolCallId] {
        &self.pending_tool_calls
    }

    /// Approval keys the user chose to remember for this session.
    #[must_use]
    pub fn remembered_approvals(&self) -> &[String] {
        &self.remembered_approvals
    }

    /// Synthesize results for calls that never completed.
    ///
    /// [`SessionState::replay`] calls this for you. Call it directly only if you folded
    /// events yourself with [`SessionState::apply`] and have now reached the end of the
    /// log — otherwise a call that is legitimately still in flight would be closed early.
    ///
    /// Idempotent.
    pub fn close_interrupted_calls(&mut self) {
        for call_id in std::mem::take(&mut self.pending_tool_calls) {
            self.messages.push(Message {
                role: crate::model::Role::Tool,
                content: vec![crate::model::ContentBlock::ToolResult {
                    call_id,
                    content: "The runtime stopped before this tool finished. \
                              Its effects, if any, are unknown; verify before retrying."
                        .to_string(),
                    is_error: true,
                }],
            });
        }
    }

    /// Apply one event.
    ///
    /// This is the incremental path, for a live runtime folding events as they are
    /// written. It deliberately leaves an unanswered tool call *pending*, because during
    /// a live run such a call really is still running. When you reach the end of a stored
    /// log instead, use [`SessionState::replay`] — or call
    /// [`SessionState::close_interrupted_calls`] yourself.
    pub fn apply(&mut self, stored: &StoredEvent) {
        self.last_seq = stored.seq;
        match &stored.event {
            SessionEvent::UserMessage { message } => self.messages.push(message.clone()),
            SessionEvent::AssistantMessage { message, usage, .. } => {
                self.messages.push(message.clone());
                self.total_usage.input_tokens += usage.input_tokens;
                self.total_usage.output_tokens += usage.output_tokens;
                self.total_usage.cache_read_tokens += usage.cache_read_tokens;
                self.total_usage.cache_write_tokens += usage.cache_write_tokens;
            }
            SessionEvent::ToolCalled { call, .. } => {
                // Not projected into `messages` -- the call already lives in the preceding
                // assistant message. Tracked here so an unanswered call is detectable.
                self.pending_tool_calls.push(call.id);
            }
            SessionEvent::ToolCompleted {
                call_id, result, ..
            } => {
                self.pending_tool_calls.retain(|id| id != call_id);
                self.messages.push(Message {
                    role: crate::model::Role::Tool,
                    content: vec![crate::model::ContentBlock::ToolResult {
                        call_id: *call_id,
                        content: result.content.clone(),
                        is_error: result.is_error,
                    }],
                });
            }
            SessionEvent::ToolBlocked {
                call_id, reason, ..
            } => {
                self.pending_tool_calls.retain(|id| id != call_id);
                // The model must see the refusal, or it will loop retrying the same call.
                self.messages.push(Message {
                    role: crate::model::Role::Tool,
                    content: vec![crate::model::ContentBlock::ToolResult {
                        call_id: *call_id,
                        content: format!("Blocked by policy: {reason}"),
                        is_error: true,
                    }],
                });
            }
            SessionEvent::ApprovalResolved {
                scope_key,
                outcome,
                remembered,
                ..
            } => {
                if *remembered && *outcome == crate::policy::ApprovalOutcome::ApprovedForSession {
                    self.remembered_approvals.push(scope_key.clone());
                }
            }
            SessionEvent::Checkpoint { summary, .. } => {
                // A checkpoint discards the assistant message that issued any outstanding
                // call, so its synthetic result would arrive with nothing to answer --
                // and a tool result with no preceding tool call is a provider 400.
                self.pending_tool_calls.clear();
                // The summary is projected as a message, not merely stashed in a field.
                // If it lived only in `checkpoint_summary`, an assembler that forgot to
                // read that field would hand the model total amnesia -- and a second
                // checkpoint would overwrite the first, losing the earlier history for
                // good.
                self.messages.clear();
                self.messages.push(Message {
                    role: crate::model::Role::User,
                    content: vec![crate::model::ContentBlock::text(format!(
                        "[Summary of earlier conversation]\n{summary}"
                    ))],
                });
                self.checkpoint_summary = Some(summary.clone());
            }
            SessionEvent::Closed { .. } => self.closed = true,
            SessionEvent::ApprovalRequested { .. }
            | SessionEvent::Created { .. }
            | SessionEvent::RunStarted { .. }
            | SessionEvent::ModelRequested { .. }
            | SessionEvent::RunCompleted { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ContentBlock, Role};

    fn stored(seq: u64, event: SessionEvent) -> StoredEvent {
        StoredEvent {
            seq,
            at: Timestamp::from_millis(i64::try_from(seq).unwrap()).unwrap(),
            event,
        }
    }

    /// The session log is a persistence format read by future versions of Rivet and
    /// by external tooling. If this test fails, someone renamed a Rust variant and
    /// invalidated every session log in existence -- restore the `#[serde(rename)]`
    /// rather than updating this list.
    #[test]
    fn wire_names_are_pinned() {
        let cases: Vec<(SessionEvent, &str)> = vec![
            (
                SessionEvent::Created {
                    workspace_root: "/repo".into(),
                    parent: None,
                },
                "session.created",
            ),
            (
                SessionEvent::UserMessage {
                    message: Message::user("hi"),
                },
                "user.message",
            ),
            (
                SessionEvent::ToolBlocked {
                    run_id: RunId::new(),
                    call_id: ToolCallId::new(),
                    policy: "default".into(),
                    reason: "no".into(),
                },
                "tool.blocked",
            ),
            (
                SessionEvent::Checkpoint {
                    summary: "s".into(),
                    through_seq: 1,
                },
                "session.checkpoint",
            ),
            (
                SessionEvent::Closed {
                    reason: "done".into(),
                },
                "session.closed",
            ),
        ];
        for (event, expected) in cases {
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(
                json["type"], expected,
                "wire name drifted; old session logs would stop parsing"
            );
        }
    }

    #[test]
    fn a_persisted_event_still_parses() {
        // Hand-written, as an older Rivet would have produced it.
        let raw = r#"{"type":"user.message","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#;
        let event: SessionEvent = serde_json::from_str(raw).expect("old logs must still load");
        match event {
            SessionEvent::UserMessage { message } => assert_eq!(message.text(), "hi"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn replay_reconstructs_the_conversation_in_order() {
        let events = vec![
            stored(
                1,
                SessionEvent::Created {
                    workspace_root: "/repo".into(),
                    parent: None,
                },
            ),
            stored(
                2,
                SessionEvent::UserMessage {
                    message: Message::user("hi"),
                },
            ),
            stored(
                3,
                SessionEvent::AssistantMessage {
                    run_id: RunId::new(),
                    message: Message::assistant("hello"),
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 3,
                        ..Usage::default()
                    },
                },
            ),
        ];
        let state = SessionState::replay(&events);
        assert_eq!(state.messages.len(), 2);
        assert_eq!(state.messages[0].text(), "hi");
        assert_eq!(state.messages[1].text(), "hello");
        assert_eq!(state.total_usage.input_tokens, 10);
        assert_eq!(state.last_seq, 3);
    }

    #[test]
    fn replay_is_deterministic() {
        let events = vec![
            stored(
                1,
                SessionEvent::UserMessage {
                    message: Message::user("a"),
                },
            ),
            stored(
                2,
                SessionEvent::UserMessage {
                    message: Message::user("b"),
                },
            ),
        ];
        let first = SessionState::replay(&events);
        let second = SessionState::replay(&events);
        assert_eq!(first.messages, second.messages);
        assert_eq!(first.last_seq, second.last_seq);
    }

    #[test]
    fn a_blocked_tool_still_produces_a_result_for_the_model() {
        let call_id = ToolCallId::new();
        let events = vec![stored(
            1,
            SessionEvent::ToolBlocked {
                run_id: RunId::new(),
                call_id,
                policy: "default".into(),
                reason: "rm -rf is destructive".into(),
            },
        )];
        let state = SessionState::replay(&events);
        assert_eq!(
            state.messages.len(),
            1,
            "model must see the refusal, not silence"
        );
        match &state.messages[0].content[0] {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(is_error);
                assert!(content.contains("rm -rf is destructive"));
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
        assert_eq!(state.messages[0].role, Role::Tool);
    }

    #[test]
    fn a_checkpoint_folds_history_into_a_message_not_just_a_field() {
        let events = vec![
            stored(
                1,
                SessionEvent::UserMessage {
                    message: Message::user("old"),
                },
            ),
            stored(
                2,
                SessionEvent::Checkpoint {
                    summary: "did stuff".into(),
                    through_seq: 1,
                },
            ),
            stored(
                3,
                SessionEvent::UserMessage {
                    message: Message::user("new"),
                },
            ),
        ];
        let state = SessionState::replay(&events);
        assert_eq!(state.messages.len(), 2, "summary must survive as a message");
        assert!(
            state.messages[0].text().contains("did stuff"),
            "an assembler that ignores `checkpoint_summary` must not cause amnesia"
        );
        assert_eq!(state.messages[1].text(), "new");
        assert_eq!(state.checkpoint_summary.as_deref(), Some("did stuff"));
    }

    #[test]
    fn a_second_checkpoint_does_not_erase_the_first() {
        let events = vec![
            stored(
                1,
                SessionEvent::Checkpoint {
                    summary: "phase one".into(),
                    through_seq: 0,
                },
            ),
            stored(
                2,
                SessionEvent::UserMessage {
                    message: Message::user("more"),
                },
            ),
            stored(
                3,
                // A well-behaved compactor folds the previous summary into the new one.
                SessionEvent::Checkpoint {
                    summary: "phase one; then more".into(),
                    through_seq: 2,
                },
            ),
        ];
        let state = SessionState::replay(&events);
        assert!(state.messages[0].text().contains("phase one"));
    }

    #[test]
    fn a_crash_between_call_and_result_still_replays_into_a_valid_conversation() {
        // This is the exact shape a `kill -9` mid-tool-call leaves behind.
        let call_id = ToolCallId::new();
        let run_id = RunId::new();
        let call = crate::tool::ToolCall {
            id: call_id,
            name: "shell".into(),
            input: serde_json::json!({ "command": "cargo test" }),
        };
        let events = vec![
            stored(
                1,
                SessionEvent::UserMessage {
                    message: Message::user("run the tests"),
                },
            ),
            stored(
                2,
                SessionEvent::AssistantMessage {
                    run_id,
                    message: Message {
                        role: Role::Assistant,
                        content: vec![ContentBlock::ToolCall(call.clone())],
                    },
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                },
            ),
            stored(3, SessionEvent::ToolCalled { run_id, call }),
            // ...and then the process died. No ToolCompleted.
        ];

        let state = SessionState::replay(&events);

        assert!(
            state.pending_tool_calls().is_empty(),
            "replay must resolve the dangling call, not leave it pending"
        );
        let last = state.messages.last().expect("a synthetic result");
        assert_eq!(last.role, Role::Tool);
        match &last.content[0] {
            ContentBlock::ToolResult {
                call_id: id,
                is_error,
                content,
            } => {
                assert_eq!(*id, call_id);
                assert!(is_error);
                assert!(
                    content.contains("stopped before this tool finished"),
                    "{content}"
                );
            }
            other => panic!("expected a synthetic tool result, got {other:?}"),
        }
    }

    #[test]
    fn incremental_apply_keeps_an_in_flight_call_pending() {
        // During a live run an unanswered call is genuinely still running, so `apply`
        // must not close it. Only reaching the end of a stored log means "interrupted".
        let call_id = ToolCallId::new();
        let run_id = RunId::new();
        let call = crate::tool::ToolCall {
            id: call_id,
            name: "shell".into(),
            input: serde_json::json!({}),
        };
        let event = stored(1, SessionEvent::ToolCalled { run_id, call });

        let mut state = SessionState::default();
        state.apply(&event);
        assert_eq!(state.pending_tool_calls(), [call_id], "still in flight");

        state.close_interrupted_calls();
        assert!(state.pending_tool_calls().is_empty());
        assert_eq!(
            state.messages.len(),
            1,
            "now closed with a synthetic result"
        );
    }

    #[test]
    fn a_completed_call_leaves_nothing_pending() {
        let call_id = ToolCallId::new();
        let run_id = RunId::new();
        let call = crate::tool::ToolCall {
            id: call_id,
            name: "shell".into(),
            input: serde_json::json!({}),
        };
        let events = vec![
            stored(1, SessionEvent::ToolCalled { run_id, call }),
            stored(
                2,
                SessionEvent::ToolCompleted {
                    run_id,
                    call_id,
                    result: crate::tool::ToolResult::ok("all green"),
                    duration_ms: 12,
                },
            ),
        ];
        let state = SessionState::replay(&events);
        assert!(state.pending_tool_calls().is_empty());
        assert_eq!(state.messages.len(), 1, "exactly one result, not two");
    }

    #[test]
    fn a_blocked_call_also_clears_the_pending_entry() {
        let call_id = ToolCallId::new();
        let run_id = RunId::new();
        let call = crate::tool::ToolCall {
            id: call_id,
            name: "shell".into(),
            input: serde_json::json!({}),
        };
        let events = vec![
            stored(1, SessionEvent::ToolCalled { run_id, call }),
            stored(
                2,
                SessionEvent::ToolBlocked {
                    run_id,
                    call_id,
                    policy: "default".into(),
                    reason: "destructive".into(),
                },
            ),
        ];
        let state = SessionState::replay(&events);
        assert!(state.pending_tool_calls().is_empty());
        assert_eq!(state.messages.len(), 1, "the refusal, and only the refusal");
    }

    #[test]
    fn a_remembered_approval_survives_a_resume() {
        let call_id = ToolCallId::new();
        let events = vec![
            stored(
                1,
                SessionEvent::ApprovalRequested {
                    run_id: RunId::new(),
                    call_id,
                    reason: "destructive".into(),
                    preview: "rm -rf build".into(),
                    scope_key: "shell:rm-rf".into(),
                },
            ),
            stored(
                2,
                SessionEvent::ApprovalResolved {
                    run_id: RunId::new(),
                    call_id,
                    scope_key: "shell:rm-rf".into(),
                    outcome: crate::policy::ApprovalOutcome::ApprovedForSession,
                    remembered: true,
                    actor: Some("kangwoo".into()),
                },
            ),
        ];
        let state = SessionState::replay(&events);
        assert_eq!(
            state.remembered_approvals(),
            ["shell:rm-rf".to_string()],
            "a session-scoped approval must be a projection of the log, not memory"
        );
        assert!(
            state.is_pre_approved("shell:rm-rf"),
            "a later call with a different id must still match the remembered grant"
        );
        assert!(!state.is_pre_approved("shell:curl"));
    }

    #[test]
    fn a_one_off_approval_is_not_remembered() {
        let events = vec![stored(
            1,
            SessionEvent::ApprovalResolved {
                run_id: RunId::new(),
                call_id: ToolCallId::new(),
                scope_key: "shell:rm-rf".into(),
                outcome: crate::policy::ApprovalOutcome::Approved,
                remembered: false,
                actor: Some("kangwoo".into()),
            },
        )];
        assert!(
            SessionState::replay(&events)
                .remembered_approvals()
                .is_empty()
        );
    }
}
