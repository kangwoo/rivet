//! Tool dispatch: the fixed pipeline between "the model asked" and "it happened".
//!
//! ```text
//!  1 Resolve      registry lookup           unknown -> an error result the model can read
//!  2 Scope        agent.allows_tool()       outside -> ToolBlocked
//!  3 Validate     schema::validate          invalid -> an error result the model can fix
//!  4 Intercept    policy_chain::evaluate    concurrent, one timeout each
//!  5 Policy       policy_chain::evaluate    all of them, folded most-restrictive-wins
//!  6 Approval     approval::Approvals       remembered -> unattended -> ask
//!    -- session: tool.called --
//!  7 Sandbox      SandboxScope::resolve     a lookup, never a refusal
//!  8 Execute      spawn + timeout + cancel
//!  9 Truncate     max_output_bytes
//! 10 Persist      tool.completed | tool.blocked
//!    -- teardown() on every path out of 7, 8 and 9 --
//! ```
//!
//! Two things about this order are load-bearing. **Validation precedes policy** so a
//! policy always reads well-formed input; the alternative is every policy reimplementing
//! defensive parsing, and one of them getting it wrong. And steps 4 to 6 sit *between*
//! validation and the durable `tool.called`, so nothing is recorded as called until the
//! chain and the approval have both had their say.
//!
//! Step 7 is the one that sits *after* `tool.called`, and deliberately. It is a registry
//! lookup that cannot refuse anything, and putting it after the last fallible step means the
//! region from the scope's construction to its `teardown()` contains no `?` — so "teardown
//! on every path" is a property of the shape of this function rather than of an argument
//! about another module's laziness.
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
use futures_util::FutureExt;
use rivet_core::agent::AgentSpec;
use rivet_core::capability::PermissionSet;
use rivet_core::error::{Error, ErrorKind};
use rivet_core::event::{Event, EventBus, EventEnvelope, ToolEvent};
use rivet_core::id::{AgentId, RunId, SessionId, ToolCallId};
use rivet_core::policy::{
    ApprovalOutcome, ExecutionConstraints, Outcome, PolicyAction, PolicyRequest,
};
use rivet_core::sandbox::{ExecOutput, ExecSpec, SandboxRequest};
use rivet_core::session::SessionEvent;
use rivet_core::tool::{ToolCall, ToolContext, ToolContextData, ToolHost, ToolResult, Truncation};
use rivet_core::workspace::Workspace;
use tokio_util::sync::CancellationToken;

use crate::approval::{Approvals, Ask, Where};
use crate::policy_chain::{self, Baseline};
use crate::registry::{RegisteredTool, Registry};
use crate::sandbox_scope::SandboxScope;
use crate::schema;
use crate::session_log::SessionWriter;
use crate::session_recovery::{INTERRUPTED_TOOL_RESULT, NOT_STARTED_TOOL_RESULT};

/// Default cap on tool output, matching the contract's own test value.
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 65_536;

/// How long a cancelled tool has to come back on its own before it is abandoned.
///
/// Part of the five-second budget: cooperative first, then give up. Waiting forever on a
/// tool that ignores cancellation turns "five seconds" from a promise into a hope.
pub const DEFAULT_CANCEL_GRACE: Duration = Duration::from_millis(2_000);

/// The policy name recorded when a **tool itself** refuses.
///
/// This is the refusal `Workspace::resolve`, [`crate::fsguard`] and [`crate::argv`] raise
/// from inside step 8, after the chain has already allowed the call. It stays distinct from
/// a chain decision, which records the name of whatever actually decided
/// ([`crate::policy_chain::Evaluated::deciding`]): "the tool's own containment refused this
/// while it was running" and "a policy refused this call before it ran" are different facts,
/// and an audit that could not tell them apart would be looking in the wrong place.
///
/// `"tool"` and not `"workspace"`. The dispatcher cannot tell an escaping path from an argv
/// refusal at this point — both arrive as one `ErrorKind::PolicyDenied` out of `execute` —
/// and `default.workspace` is a real policy in the chain that never saw this call. Naming
/// the label after it filed the fact under a policy that did not produce it, which is a
/// weaker version of the argument for raising `PolicyDenied` here at all.
pub const TOOL_POLICY: &str = "tool";

/// The policy name recorded when a call is outside the agent's tool scope (step 2).
pub const SCOPE_POLICY: &str = "agent.scope";

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
    /// Named profile in force. Reaches every `PolicyRequest` the chain builds.
    pub profile: String,
    /// True when nothing can ask a human. Step 6 turns an approval into a refusal on it.
    pub unattended: bool,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: u64,
    /// `[sandbox] provider`, as a **default**. This call's provider is
    /// `constraints.sandbox` if a policy named one, and this otherwise.
    ///
    /// Not a constraint: `ExecutionConstraints::merge` resolves that axis left-to-right,
    /// so a seeded name would beat a policy that asked for a different one. Failing to
    /// resolve it does not block a call — only starting a process does. See
    /// [`crate::sandbox_scope`].
    pub sandbox_provider: Option<String>,
    /// Where an approval goes, and what has already been granted for this session. The
    /// memory is a projection of the session log, not runtime state.
    pub approvals: Approvals,
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

        // 1 Resolve, 2 Scope, 3 Validate. All three answer "is this callable at all",
        // and all three answer it without a policy having been consulted.
        let registered = match self.admit(ctx, &call).await {
            Ok(registered) => registered,
            Err(disposition) => return self.finish(ctx, session, &call, 0, disposition).await,
        };
        // The spec fixed at registration, not asked for again: the schema the validator
        // checked and the annotations the chain is about to read have to be one value.
        let spec = registered.spec.clone();

        // 4 Intercept, 5 Policy, 6 Approval. Pulled out because the three of them are one
        // question -- "may this call happen, and in what form" -- and because a settled
        // answer here means the call is over before step 7 exists.
        let (call, constraints) = match self.judge(ctx, session, &spec, call).await? {
            Judged::Proceed { call, constraints } => (call, constraints),
            Judged::Settled { call, disposition } => {
                return self.finish(ctx, session, &call, 0, disposition).await;
            }
        };

        // The durable record that this call happened comes **before** the scope exists.
        //
        // Not a detail of ordering: nothing between the scope's construction and the
        // teardown below may return early, or there is a path out of `dispatch` that
        // releases nothing. This `?` is the only fallible step in the region, so it is
        // hoisted above the scope rather than guarded. The alternative -- leaving it where
        // it reads more naturally and arguing that `prepare` is lazy, so the scope is
        // provably `Idle` and there is nothing to release -- is true today and true by a
        // fact about another module. `docs/plan.md`'s risk note asks for teardown on *every*
        // path, and a rule that holds because of something elsewhere is a rule that stops
        // holding when that something changes.
        session
            .append(SessionEvent::ToolCalled {
                run_id: ctx.run_id,
                call: call.clone(),
            })
            .await?;

        // 7 Sandbox. A lookup, not a gate: `constraints.sandbox` if a policy named one,
        // the configured default otherwise, and a miss is recorded rather than refused.
        //
        // From here to `scope.teardown()` there is no `?` and no `return`.
        let scope = Arc::new(
            SandboxScope::resolve(
                &self.registry,
                constraints
                    .sandbox
                    .as_deref()
                    .or(ctx.sandbox_provider.as_deref()),
                SandboxRequest {
                    workspace: ctx.workspace.clone(),
                    permissions: constraints
                        .permissions
                        .clone()
                        .unwrap_or_else(|| ctx.permissions.clone()),
                    options: serde_json::Map::new(),
                },
            )
            .await,
        );
        self.publish(
            ctx,
            ToolEvent::Started {
                call_id: call.id,
                name: call.name.clone(),
                sandboxed: scope.is_sandboxed(),
            },
        );

        // 8 Execute, then 9 Truncate -- under `catch_unwind`, so the teardown below runs
        // on the panic path too. The scope is the dispatcher's; an abandoned tool task
        // cannot take the process tree with it.
        let started = Instant::now();
        let outcome = std::panic::AssertUnwindSafe(async {
            let execution = self
                .execute(ctx, &registered, &call, &constraints, scope.clone())
                .await;
            Self::interpret(ctx, &call, &constraints, execution)
        })
        .catch_unwind()
        .await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        scope.teardown().await;

        // 10 Persist.
        let disposition = outcome.unwrap_or_else(|_| Disposition::Completed {
            result: ToolResult::error(format!(
                "tool `{}` panicked; the run continues without its result",
                call.name
            )),
            counts_as_error: true,
        });
        self.finish(ctx, session, &call, duration_ms, disposition)
            .await
    }

    /// Steps 1, 2 and 3: resolve the tool, check the agent's scope, validate the input.
    ///
    /// `Err` carries the disposition the call already has — the three refusals here are
    /// answers, not failures, and each one is something the model can act on.
    async fn admit(
        &self,
        ctx: &DispatchCtx,
        call: &ToolCall,
    ) -> Result<RegisteredTool, Disposition> {
        // 1 Resolve.
        let Some(registered) = self.registry.tool(&call.name).await else {
            let available = self.registry.tool_names().await;
            return Err(Disposition::Completed {
                result: ToolResult::error(format!(
                    "unknown tool `{}`. Available tools: {}",
                    call.name,
                    available.join(", ")
                )),
                counts_as_error: true,
            });
        };

        // 2 Scope. A reviewer that can reach `write_file` is not a reviewer.
        if !ctx.agent.allows_tool(&call.name) {
            return Err(Disposition::Blocked {
                policy: SCOPE_POLICY.to_string(),
                reason: format!(
                    "tool `{}` is not in agent `{}`'s scope",
                    call.name, ctx.agent.name
                ),
            });
        }

        // 3 Validate, before any policy sees the input. A policy that had to parse
        // defensively would be a policy that could get it wrong.
        if let Err(problems) = schema::validate(&registered.spec.input_schema, &call.input) {
            return Err(Disposition::Completed {
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
            });
        }
        Ok(registered)
    }

    /// Steps 4, 5 and 6: the chain, and the approval a `RequireApproval` needs.
    ///
    /// Returns the call to run and the limits it runs under, or the disposition the call
    /// already has. Publishing `tool.policy.evaluated` happens here because it is the
    /// chain's result, whichever way the call goes afterwards.
    ///
    /// # Errors
    /// Only when the session log cannot be written.
    async fn judge(
        &self,
        ctx: &DispatchCtx,
        session: &SessionWriter,
        spec: &rivet_core::tool::ToolSpec,
        call: ToolCall,
    ) -> rivet_core::Result<Judged> {
        let baseline = Baseline::new(ExecutionConstraints {
            timeout_ms: ctx.timeout_ms,
            sandbox: None,
            max_output_bytes: Some(ctx.max_output_bytes),
            permissions: Some(ctx.permissions.clone()),
        });
        let request = PolicyRequest {
            action: PolicyAction::ToolCall {
                call: call.clone(),
                annotations: spec.annotations.clone(),
            },
            session_id: ctx.session_id,
            agent_id: ctx.agent_id,
            run_id: ctx.run_id,
            workspace: ctx.workspace.clone(),
            permissions: ctx.permissions.clone(),
            profile: ctx.profile.clone(),
            unattended: ctx.unattended,
        };
        // The chain as a whole races cancellation. A run that is already over must not sit
        // through an interceptor's budget, and a call that never got a decision never
        // started -- which is a different fact from being refused.
        let evaluated = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => {
                return Ok(Judged::Settled { call, disposition: not_started() });
            }
            evaluated = policy_chain::evaluate(
                &self.registry, spec, request, baseline, &ctx.cancel,
            ) => evaluated,
        };
        self.publish(
            ctx,
            ToolEvent::PolicyEvaluated {
                call_id: call.id,
                decision: Box::new(evaluated.decision.clone()),
                policy: evaluated.deciding.clone(),
            },
        );

        // A narrowed call is the one that runs, and the one the log records as called.
        let call = evaluated.decision.rewrite.clone().unwrap_or(call);
        let constraints = evaluated.decision.constraints.clone();

        // 6 Approval. Only `RequireApproval` reaches step 6; the other two outcomes are
        // already answers.
        let ask = match &evaluated.decision.outcome {
            Outcome::Allow => return Ok(Judged::Proceed { call, constraints }),
            Outcome::Deny { reason } => {
                return Ok(Judged::Settled {
                    disposition: Disposition::Blocked {
                        policy: evaluated.deciding.clone(),
                        reason: reason.clone(),
                    },
                    call,
                });
            }
            Outcome::RequireApproval { .. } => Ask::from_outcome(&evaluated.decision.outcome)
                .expect("the arm matched RequireApproval"),
        };

        let outcome = ctx
            .approvals
            .resolve(
                session,
                &self.bus,
                Where {
                    session_id: ctx.session_id,
                    run_id: ctx.run_id,
                    call_id: call.id,
                },
                &ask,
                ctx.unattended,
                &ctx.cancel,
            )
            .await?;
        Ok(match outcome {
            ApprovalOutcome::Approved | ApprovalOutcome::ApprovedForSession => {
                Judged::Proceed { call, constraints }
            }
            ApprovalOutcome::Denied => Judged::Settled {
                disposition: Disposition::Blocked {
                    policy: evaluated.deciding.clone(),
                    reason: format!("{}; approval was refused", ask.reason),
                },
                call,
            },
            // Nobody answered before the run ended. The call provably had no effect, so it
            // reads as "never started" rather than as a refusal -- and it does not advance
            // the consecutive-error counter.
            ApprovalOutcome::TimedOut => Judged::Settled {
                call,
                disposition: not_started(),
            },
        })
    }

    /// Step 8: run the tool, honoring the timeout and the cancellation grace period.
    ///
    /// `constraints` rather than `ctx` decides the budget and the grant: the chain has
    /// already folded the host's baseline with whatever the policies asked for, and its
    /// answer is what the tool and its sandbox both see.
    async fn execute(
        &self,
        ctx: &DispatchCtx,
        registered: &RegisteredTool,
        call: &ToolCall,
        constraints: &ExecutionConstraints,
        scope: Arc<SandboxScope>,
    ) -> Execution {
        // A child of the *effective* token, so a run deadline reaches a running tool.
        let cancel = ctx.cancel.child_token();
        let host = Arc::new(RuntimeToolHost {
            bus: self.bus.clone(),
            session_id: ctx.session_id,
            run_id: ctx.run_id,
            call_id: call.id,
            cancel: cancel.clone(),
            scope,
        });
        let tool_ctx = ToolContext::new(
            ToolContextData {
                session_id: ctx.session_id,
                agent_id: ctx.agent_id,
                run_id: ctx.run_id,
                call_id: call.id,
                workspace: ctx.workspace.clone(),
                permissions: constraints
                    .permissions
                    .clone()
                    .unwrap_or_else(|| ctx.permissions.clone()),
                timeout_ms: constraints.timeout_ms.or(ctx.timeout_ms),
                max_output_bytes: Some(max_output_bytes(ctx, constraints)),
            },
            host,
        );

        let tool = registered.tool.clone();
        let input = call.input.clone();
        // Spawned so a panicking tool becomes a `JoinError` instead of unwinding the run.
        let mut handle = tokio::spawn(async move { tool.execute(tool_ctx, input).await });

        let timeout = constraints
            .timeout_ms
            .or(ctx.timeout_ms)
            .map(Duration::from_millis);
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
    fn interpret(
        ctx: &DispatchCtx,
        call: &ToolCall,
        constraints: &ExecutionConstraints,
        execution: Execution,
    ) -> Disposition {
        let timeout_ms = constraints
            .timeout_ms
            .or(ctx.timeout_ms)
            .unwrap_or_default();
        match execution {
            Execution::Returned(Ok(result)) => {
                let counts_as_error = result.is_error;
                Disposition::Completed {
                    result: truncate(result, max_output_bytes(ctx, constraints)),
                    counts_as_error,
                }
            }
            Execution::Returned(Err(error)) => match error.kind() {
                // A refusal is a durable audit fact, not a tool failure.
                ErrorKind::PolicyDenied | ErrorKind::ApprovalDenied => Disposition::Blocked {
                    policy: TOOL_POLICY.to_string(),
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

/// A call that ended before step 8: the chain was cancelled, or nobody answered its
/// approval before the run did.
///
/// Deliberately not `Blocked`. The call provably had no effect, and `Blocked` advances the
/// consecutive-error counter — which would let one Ctrl-C during an approval prompt push a
/// session toward a limit it never earned. The log still distinguishes the two: an
/// `approval.resolved` with `outcome: "timed_out"` is a wait that was abandoned, and
/// `"denied"` is a person saying no.
fn not_started() -> Disposition {
    Disposition::Completed {
        result: ToolResult::error(NOT_STARTED_TOOL_RESULT),
        counts_as_error: false,
    }
}

/// The output cap this call runs under: the chain's, or the host's if it said nothing.
fn max_output_bytes(ctx: &DispatchCtx, constraints: &ExecutionConstraints) -> u64 {
    constraints.max_output_bytes.unwrap_or(ctx.max_output_bytes)
}

/// What steps 4 to 6 concluded.
enum Judged {
    /// Run this call, under these limits.
    Proceed {
        call: ToolCall,
        constraints: ExecutionConstraints,
    },
    /// The call is over; this is what to record for it.
    Settled {
        call: ToolCall,
        disposition: Disposition,
    },
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
    const MARKER: &str = "\n… [output truncated]";
    let original = result.content.len() as u64;
    if original <= max_bytes {
        return result;
    }
    // The marker is part of what the caller receives, so it comes out of the budget
    // rather than sitting on top of it. Otherwise `max_output_bytes` is not a bound and
    // `retained_bytes` describes something other than what is in `content`.
    let cap = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut end = cap.saturating_sub(MARKER.len());
    while end > 0 && !result.content.is_char_boundary(end) {
        end -= 1;
    }
    result.content.truncate(end);
    result.content.push_str(MARKER);
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
    /// Shared with the dispatcher, which is the half that releases it. A tool task that is
    /// abandoned keeps this `Arc` alive; the dispatcher tears the scope down regardless.
    scope: Arc<SandboxScope>,
}

impl RuntimeToolHost {
    #[must_use]
    pub fn new(
        bus: Arc<dyn EventBus>,
        session_id: SessionId,
        run_id: RunId,
        call_id: ToolCallId,
        cancel: CancellationToken,
        scope: Arc<SandboxScope>,
    ) -> Self {
        Self {
            bus,
            session_id,
            run_id,
            call_id,
            cancel,
            scope,
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

    /// Runs the process under whatever confinement step 7 resolved.
    ///
    /// The one place a missing sandbox stops anything: if the decision named a provider
    /// nothing registered, this is where the call finds out, by name. A call that never
    /// gets here is never blocked for want of one.
    async fn exec(&self, spec: ExecSpec) -> rivet_core::Result<ExecOutput> {
        self.scope.exec(spec, self.cancel.clone()).await
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
    fn truncation_stays_inside_max_output_bytes() {
        // The marker is part of what the caller receives, so a cap that excludes it is
        // not a cap. `retained_bytes` has to describe `content`, marker and all.
        for cap in [40u64, 64, 200] {
            let result = truncate(ToolResult::ok("x".repeat(1_000)), cap);
            assert!(
                result.content.len() as u64 <= cap,
                "cap {cap}: content is {} bytes",
                result.content.len()
            );
            let truncation = result.truncated.expect("truncated");
            assert_eq!(
                truncation.retained_bytes,
                (result.content.len() - "\n… [output truncated]".len()) as u64,
                "cap {cap}: retained_bytes must describe the content actually returned"
            );
        }
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
