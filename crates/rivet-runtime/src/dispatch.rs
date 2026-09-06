//! Tool dispatch: the fixed pipeline between "the model asked" and "it happened".
//!
//! ```text
//!  1 Resolve      registry lookup           unknown -> an error result the model can read
//!  2 Scope        agent.allows_tool()       outside -> ToolBlocked
//!  3 Validate     schema::validate          invalid -> an error result the model can fix
//!  4 Intercept    (Phase 4)                 no-op, position fixed
//!  5 Policy       (Phase 4)                 no-op, position fixed
//!  6 Approval     (Phase 4)                 no-op, position fixed
//!  7 Sandbox      (Phase 4)                 no-op, position fixed
//!    -- session: tool.called --
//!  8 Execute      spawn + timeout + cancel
//!  9 Truncate     max_output_bytes
//! 10 Persist      tool.completed | tool.blocked
//! ```
//!
//! Two things about this order are load-bearing. **Validation precedes policy** so a
//! policy always reads well-formed input; the alternative is every policy reimplementing
//! defensive parsing, and one of them getting it wrong. And the Phase 4 steps are *empty
//! but present*: filling them later must not require re-deciding where they go.
//!
//! # Exactly one terminating event per accepted call
//!
//! Every call that reaches this dispatcher leaves exactly one `tool.completed` or
//! `tool.blocked` in the log — cancellation included. Skipping it would leave an
//! unanswered `tool_calls` in the message array, which every major provider rejects with a
//! 400, so a correctly cancelled run would be a session that cannot be resumed.
//!
//! That is also why the cancellation grace period lives **inside** step 8 rather than in
//! the loop: whoever gives up on a tool must be the one that writes its result, and the
//! dispatcher is the side holding the session writer.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rivet_core::agent::AgentSpec;
use rivet_core::capability::PermissionSet;
use rivet_core::error::{Capability, Error, ErrorKind};
use rivet_core::event::{Event, EventBus, EventEnvelope, ToolEvent};
use rivet_core::id::{AgentId, RunId, SessionId, ToolCallId};
use rivet_core::sandbox::{ExecOutput, ExecSpec};
use rivet_core::session::SessionEvent;
use rivet_core::tool::{
    Tool, ToolCall, ToolContext, ToolContextData, ToolHost, ToolResult, Truncation,
};
use rivet_core::workspace::Workspace;
use tokio_util::sync::CancellationToken;

use crate::registry::Registry;
use crate::schema;
use crate::session_log::SessionWriter;
use crate::session_recovery::INTERRUPTED_TOOL_RESULT;

/// Default cap on tool output, matching the contract's own test value.
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 65_536;

/// How long a cancelled tool has to come back on its own before it is abandoned.
///
/// Part of the five-second budget: cooperative first, then give up. Waiting forever on a
/// tool that ignores cancellation turns "five seconds" from a promise into a hope.
pub const DEFAULT_CANCEL_GRACE: Duration = Duration::from_millis(2_000);

/// The policy name recorded when workspace containment refuses a path.
///
/// Phase 1 has no policy chain; the only denials come from `Workspace::resolve`, and an
/// audit should say so rather than name a policy that does not exist yet.
pub const WORKSPACE_POLICY: &str = "workspace";

/// What became of one call.
#[derive(Clone, Debug)]
pub enum Disposition {
    Completed {
        result: ToolResult,
        /// Whether this advances `max_consecutive_tool_errors`.
        ///
        /// A cancelled tool is not a failing tool: counting it would let one Ctrl-C on a
        /// long run push a session toward a limit it never earned.
        counts_as_error: bool,
    },
    Blocked {
        policy: String,
        reason: String,
    },
}

impl Disposition {
    /// The text the model will see for this call.
    #[must_use]
    pub fn content(&self) -> String {
        match self {
            Self::Completed { result, .. } => result.content.clone(),
            Self::Blocked { reason, .. } => format!("Blocked by policy: {reason}"),
        }
    }

    /// Whether the model sees this as a failure.
    ///
    /// Distinct from [`Disposition::counts_as_error`]: an interrupted call reads as an
    /// error to the model (it did not produce what was asked) without being the tool's
    /// fault.
    #[must_use]
    pub fn is_error(&self) -> bool {
        match self {
            Self::Completed { result, .. } => result.is_error,
            Self::Blocked { .. } => true,
        }
    }

    /// Whether this outcome advances the consecutive-tool-error counter.
    #[must_use]
    pub fn counts_as_error(&self) -> bool {
        match self {
            Self::Completed {
                counts_as_error, ..
            } => *counts_as_error,
            Self::Blocked { .. } => true,
        }
    }
}

/// Everything one dispatch needs that is not the call itself.
#[derive(Clone, Debug)]
pub struct DispatchCtx {
    pub agent: Arc<AgentSpec>,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub run_id: RunId,
    pub workspace: Workspace,
    /// The grant the profile computed. Phase 2 intersects it with each plugin's manifest;
    /// this is the `profile` operand of that meet.
    pub permissions: PermissionSet,
    /// Named profile in force. Carried so Phase 4 does not have to re-plumb it.
    pub profile: String,
    /// True when nothing can ask a human. Carried for the same reason.
    pub unattended: bool,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: u64,
    /// The *effective* cancellation token: user cancellation or the run deadline. Each
    /// call gets a child of it, so a deadline reaches a tool that is already running.
    pub cancel: CancellationToken,
    pub grace: Duration,
}

/// Runs tool calls through the pipeline.
#[derive(Clone)]
pub struct ToolDispatcher {
    registry: Registry,
    bus: Arc<dyn EventBus>,
}

impl fmt::Debug for ToolDispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ToolDispatcher")
    }
}

impl ToolDispatcher {
    #[must_use]
    pub fn new(registry: Registry, bus: Arc<dyn EventBus>) -> Self {
        Self { registry, bus }
    }

    /// Run one call to a durable conclusion.
    ///
    /// # Errors
    /// Only when the session log cannot be written. Every *tool* failure comes back as a
    /// [`Disposition`], because the model is the one who has to react to it.
    pub async fn dispatch(
        &self,
        ctx: &DispatchCtx,
        session: &SessionWriter,
        call: ToolCall,
    ) -> rivet_core::Result<Disposition> {
        self.publish(
            ctx,
            ToolEvent::Requested {
                call_id: call.id,
                name: call.name.clone(),
            },
        );

        // 1 Resolve.
        let Some(tool) = self.registry.tool(&call.name).await else {
            let available = self.registry.tool_names().await;
            return self
                .finish(
                    ctx,
                    session,
                    &call,
                    0,
                    Disposition::Completed {
                        result: ToolResult::error(format!(
                            "unknown tool `{}`. Available tools: {}",
                            call.name,
                            available.join(", ")
                        )),
                        counts_as_error: true,
                    },
                )
                .await;
        };
        let spec = tool.spec();

        // 2 Scope. A reviewer that can reach `write_file` is not a reviewer.
        if !ctx.agent.allows_tool(&call.name) {
            return self
                .finish(
                    ctx,
                    session,
                    &call,
                    0,
                    Disposition::Blocked {
                        policy: "agent.scope".to_string(),
                        reason: format!(
                            "tool `{}` is not in agent `{}`'s scope",
                            call.name, ctx.agent.name
                        ),
                    },
                )
                .await;
        }

        // 3 Validate, before any policy sees the input.
        if let Err(problems) = schema::validate(&spec.input_schema, &call.input) {
            return self
                .finish(
                    ctx,
                    session,
                    &call,
                    0,
                    Disposition::Completed {
                        result: ToolResult::error(format!(
                            "invalid arguments for `{}`:\n{}",
                            call.name,
                            problems
                                .iter()
                                .map(|p| format!("- {p}"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )),
                        counts_as_error: true,
                    },
                )
                .await;
        }

        // 4 Intercept, 5 Policy, 6 Approval, 7 Sandbox: Phase 4. The positions are fixed
        // here so filling them in is an edit, not a redesign.

        session
            .append(SessionEvent::ToolCalled {
                run_id: ctx.run_id,
                call: call.clone(),
            })
            .await?;
        self.publish(
            ctx,
            ToolEvent::Started {
                call_id: call.id,
                name: call.name.clone(),
                sandboxed: false,
            },
        );

        // 8 Execute.
        let started = Instant::now();
        let execution = self.execute(ctx, tool, &call).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        // 9 Truncate, then 10 Persist.
        let disposition = Self::interpret(ctx, &call, execution);
        self.finish(ctx, session, &call, duration_ms, disposition)
            .await
    }

    /// Step 8: run the tool, honoring the timeout and the cancellation grace period.
    async fn execute(&self, ctx: &DispatchCtx, tool: Arc<dyn Tool>, call: &ToolCall) -> Execution {
        // A child of the *effective* token, so a run deadline reaches a running tool.
        let cancel = ctx.cancel.child_token();
        let host = Arc::new(RuntimeToolHost {
            bus: self.bus.clone(),
            session_id: ctx.session_id,
            run_id: ctx.run_id,
            call_id: call.id,
            cancel: cancel.clone(),
        });
        let tool_ctx = ToolContext::new(
            ToolContextData {
                session_id: ctx.session_id,
                agent_id: ctx.agent_id,
                run_id: ctx.run_id,
                call_id: call.id,
                workspace: ctx.workspace.clone(),
                permissions: ctx.permissions.clone(),
                timeout_ms: ctx.timeout_ms,
                max_output_bytes: Some(ctx.max_output_bytes),
            },
            host,
        );

        let input = call.input.clone();
        // Spawned so a panicking tool becomes a `JoinError` instead of unwinding the run.
        let mut handle = tokio::spawn(async move { tool.execute(tool_ctx, input).await });

        let timeout = ctx.timeout_ms.map(Duration::from_millis);
        let stop = tokio::select! {
            joined = &mut handle => return Execution::joined(joined, None),
            () = sleep_maybe(timeout) => Stop::TimedOut,
            () = ctx.cancel.cancelled() => Stop::Cancelled,
        };

        // Ask cooperatively first. `cancel` is already tripped for `Stop::Cancelled`
        // because it is a child token; tripping it again is harmless and covers timeouts.
        cancel.cancel();
        if let Ok(joined) = tokio::time::timeout(ctx.grace, &mut handle).await {
            Execution::joined(joined, Some(stop))
        } else {
            // Abandoned, not aborted: aborting mid-write can leave a half-written file,
            // and this call's result is about to be recorded as unknown anyway. The task
            // is detached; the process is on its way out.
            drop(handle);
            Execution::Abandoned(stop)
        }
    }

    /// Turn an execution outcome into the disposition the model and the log will see.
    fn interpret(ctx: &DispatchCtx, call: &ToolCall, execution: Execution) -> Disposition {
        let timeout_ms = ctx.timeout_ms.unwrap_or_default();
        match execution {
            Execution::Returned(Ok(result)) => {
                let counts_as_error = result.is_error;
                Disposition::Completed {
                    result: truncate(result, ctx.max_output_bytes),
                    counts_as_error,
                }
            }
            Execution::Returned(Err(error)) => match error.kind() {
                // A refusal is a durable audit fact, not a tool failure.
                ErrorKind::PolicyDenied | ErrorKind::ApprovalDenied => Disposition::Blocked {
                    policy: WORKSPACE_POLICY.to_string(),
                    reason: error.message().to_string(),
                },
                ErrorKind::Cancelled => interrupted(),
                _ => Disposition::Completed {
                    result: ToolResult::error(format!("tool `{}` failed: {error}", call.name)),
                    counts_as_error: true,
                },
            },
            Execution::Panicked => Disposition::Completed {
                result: ToolResult::error(format!(
                    "tool `{}` panicked; the run continues without its result",
                    call.name
                )),
                counts_as_error: true,
            },
            Execution::Abandoned(Stop::TimedOut) | Execution::Timeout => Disposition::Completed {
                result: ToolResult::error(format!(
                    "tool `{}` exceeded {timeout_ms}ms and was stopped",
                    call.name
                )),
                counts_as_error: true,
            },
            Execution::Abandoned(Stop::Cancelled) => interrupted(),
        }
    }

    /// Steps 9 and 10: record the outcome and tell the bus.
    async fn finish(
        &self,
        ctx: &DispatchCtx,
        session: &SessionWriter,
        call: &ToolCall,
        duration_ms: u64,
        disposition: Disposition,
    ) -> rivet_core::Result<Disposition> {
        let event = match &disposition {
            Disposition::Completed { result, .. } => SessionEvent::ToolCompleted {
                run_id: ctx.run_id,
                call_id: call.id,
                result: result.clone(),
                duration_ms,
            },
            Disposition::Blocked { policy, reason } => SessionEvent::ToolBlocked {
                run_id: ctx.run_id,
                call_id: call.id,
                policy: policy.clone(),
                reason: reason.clone(),
            },
        };
        session.append(event).await?;

        match &disposition {
            Disposition::Completed { result, .. } => self.publish(
                ctx,
                ToolEvent::Completed {
                    call_id: call.id,
                    is_error: result.is_error,
                    duration_ms,
                },
            ),
            Disposition::Blocked { reason, .. } => self.publish(
                ctx,
                ToolEvent::Blocked {
                    call_id: call.id,
                    reason: reason.clone(),
                },
            ),
        }
        Ok(disposition)
    }

    fn publish(&self, ctx: &DispatchCtx, event: ToolEvent) {
        self.bus
            .publish(EventEnvelope::new(Event::Tool(event)).for_run(ctx.session_id, ctx.run_id));
    }
}

fn interrupted() -> Disposition {
    Disposition::Completed {
        result: ToolResult::error(INTERRUPTED_TOOL_RESULT),
        // Stopping a tool is not the tool failing.
        counts_as_error: false,
    }
}

/// Why the dispatcher stopped waiting.
#[derive(Clone, Copy, Debug)]
enum Stop {
    TimedOut,
    Cancelled,
}

/// How step 8 ended.
#[derive(Debug)]
enum Execution {
    Returned(rivet_core::Result<ToolResult>),
    Panicked,
    Timeout,
    /// Did not come back within the grace period after being asked to stop.
    Abandoned(Stop),
}

impl Execution {
    fn joined(
        joined: Result<rivet_core::Result<ToolResult>, tokio::task::JoinError>,
        stop: Option<Stop>,
    ) -> Self {
        match joined {
            Ok(result) => match stop {
                // It came back on its own after being asked to stop; its own answer wins.
                Some(Stop::TimedOut) if result.is_err() => Self::Timeout,
                _ => Self::Returned(result),
            },
            Err(error) if error.is_panic() => Self::Panicked,
            Err(_) => Self::Returned(Err(Error::cancelled("the tool task was cancelled"))),
        }
    }
}

/// A timeout that never fires when there is no timeout.
async fn sleep_maybe(duration: Option<Duration>) {
    match duration {
        Some(duration) => tokio::time::sleep(duration).await,
        None => std::future::pending().await,
    }
}

/// Cut output to `max_bytes`, on a character boundary, recording that it happened.
///
/// The truncation record is what lets a UI offer the full output later;
/// `artifact_ref` stays `None` until payload offloading lands.
fn truncate(mut result: ToolResult, max_bytes: u64) -> ToolResult {
    let original = result.content.len() as u64;
    if original <= max_bytes {
        return result;
    }
    let mut end = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    while end > 0 && !result.content.is_char_boundary(end) {
        end -= 1;
    }
    result.content.truncate(end);
    result.content.push_str("\n… [output truncated]");
    result.truncated = Some(Truncation {
        original_bytes: original,
        retained_bytes: end as u64,
        artifact_ref: None,
    });
    result
}

/// The live capabilities a tool gets.
///
/// Note what is absent: no registry, no session store, no runtime handle. A tool that
/// could reach back into the runtime could re-enter the loop or bypass the pipeline that
/// just admitted it.
#[derive(Debug)]
pub struct RuntimeToolHost {
    bus: Arc<dyn EventBus>,
    session_id: SessionId,
    run_id: RunId,
    call_id: ToolCallId,
    cancel: CancellationToken,
}

impl RuntimeToolHost {
    #[must_use]
    pub fn new(
        bus: Arc<dyn EventBus>,
        session_id: SessionId,
        run_id: RunId,
        call_id: ToolCallId,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            bus,
            session_id,
            run_id,
            call_id,
            cancel,
        }
    }
}

#[async_trait]
impl ToolHost for RuntimeToolHost {
    fn progress(&self, message: &str) {
        self.bus.publish(
            EventEnvelope::new(Event::Tool(ToolEvent::Progress {
                call_id: self.call_id,
                message: message.to_string(),
            }))
            .for_run(self.session_id, self.run_id),
        );
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }

    /// Refuses, on purpose.
    ///
    /// Spawning a process here would ship the exact path Phase 4 exists to gate — the
    /// sandbox, the process-group kill, the empty environment — ungated and a phase early.
    /// Phase 1 registers no tool that needs it.
    async fn exec(&self, _spec: ExecSpec) -> rivet_core::Result<ExecOutput> {
        Err(Error::new(
            ErrorKind::PolicyDenied,
            Capability::Sandbox,
            "process execution requires a sandbox provider; sandboxing lands in Phase 4",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_under_the_cap_is_untouched() {
        let result = truncate(ToolResult::ok("short"), 100);
        assert_eq!(result.content, "short");
        assert!(result.truncated.is_none());
    }

    #[test]
    fn truncation_cuts_on_a_character_boundary_and_says_so() {
        // A byte-exact cut through a multi-byte character produces invalid UTF-8 in the
        // one place we most need valid text: the model's input.
        let content = "한".repeat(100);
        let result = truncate(ToolResult::ok(content.clone()), 50);
        assert!(result.content.starts_with('한'));
        let truncation = result
            .truncated
            .expect("a UI must be able to offer the rest");
        assert_eq!(truncation.original_bytes, content.len() as u64);
        assert!(truncation.retained_bytes <= 50);
        assert!(result.content.contains("output truncated"));
    }

    #[test]
    fn cancellation_does_not_count_as_a_tool_error() {
        let disposition = interrupted();
        assert!(!disposition.counts_as_error());
        assert!(
            disposition
                .content()
                .contains("stopped before this tool finished")
        );
    }

    #[test]
    fn a_block_always_counts_as_an_error() {
        let blocked = Disposition::Blocked {
            policy: "agent.scope".into(),
            reason: "not in scope".into(),
        };
        assert!(blocked.counts_as_error());
        assert_eq!(blocked.content(), "Blocked by policy: not in scope");
    }
}
