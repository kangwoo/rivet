//! The agent loop: limits, a model round trip, tools, repeat.
//!
//! ```text
//! 1  check the five limits              -> LimitReached { limit }
//! 2  assemble context
//! 3  build the request (tools ∩ agent scope)
//! 4  session: model.requested           <- before the request goes out
//! 5  model.stream(), deltas to the bus  <- never persisted
//! 6  session: assistant.message         <- after Done, assembled
//! 7  branch on the stop reason
//! ```
//!
//! # Cancellation
//!
//! Two sources, one token. Ctrl-C and the run deadline both trip an *effective* token, and
//! every tool call receives a child of it — a deadline that could not reach a running tool
//! would be a deadline a single long tool call could blow straight through. The loop
//! records which source fired, because "the user stopped it" and "it ran out of time" are
//! different endings.
//!
//! The five-second budget:
//!
//! ```text
//! t=0     effective.cancel()
//! t<=2s   cooperative grace inside the dispatcher; a tool that returns is recorded
//! t=2s    the dispatcher gives up on it and records an interrupted result
//! t<=2.5s the loop closes this turn's *remaining* calls (see below)
//! t<=3s   session: run.completed
//! ```
//!
//! # Every call in a turn gets an answer
//!
//! When a turn is abandoned partway through a batch of tool calls, the calls that never
//! reached the dispatcher are closed here. Without that, doing cancellation *right* would
//! leave an assistant message with unanswered `tool_calls` — and every major provider
//! rejects that array with a 400, so the session could never be resumed. Cancellation,
//! the deadline, and the consecutive-error limit all share this path;
//! [`crate::session_recovery`] is the backstop for the crashes that leave no chance to run
//! it at all.
//!
//! What is *not* here: the loop body is never an arm of a `select!`. A deadline firing
//! mid-`append` would drop the write, and a session log torn in half is the one failure
//! this design refuses.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rivet_core::agent::{AgentSpec, LimitKind, RunLimits, RunSummary, StopReason};
use rivet_core::capability::PermissionSet;
use rivet_core::context::{ContextRequest, estimate_tokens};
use rivet_core::error::Error;
use rivet_core::event::{AgentEvent, Event, EventBus, EventEnvelope};
use rivet_core::id::{RunId, SessionId};
use rivet_core::model::{
    ContentBlock, Message, Model, ModelParams, ModelRequest, Role, StreamEvent, ToolChoice, Usage,
};
use rivet_core::policy::ApprovalSink;
use rivet_core::retry::{RetryDecision, RetryPolicy};
use rivet_core::session::{SessionEvent, SessionState, SessionStore};
use rivet_core::tool::{ToolCall, ToolResult, ToolSpec};
use rivet_core::workspace::Workspace;
use tokio_util::sync::CancellationToken;

use crate::approval::Approvals;
use crate::context::ContextAssembler;
use crate::digest::request_digest;
use crate::dispatch::{
    DEFAULT_CANCEL_GRACE, DEFAULT_MAX_OUTPUT_BYTES, DispatchCtx, ToolDispatcher,
};
use crate::jitter::Jitter;
use crate::registry::Registry;
use crate::session_log::SessionWriter;
use crate::session_recovery::NOT_STARTED_TOOL_RESULT;

/// Set to a directory to write out every assembled request before it is sent.
///
/// `docs/security.md` asks operators to audit what reaches a provider. The digest in
/// `model.requested` proves *which* request went out; this shows what was in it, without
/// making the prompt itself a durable session event.
pub const DUMP_REQUESTS_ENV: &str = "RIVET_DUMP_REQUESTS";

/// Everything one run needs beyond the conversation itself.
#[derive(Clone, Debug)]
pub struct RunConfig {
    pub agent: AgentSpec,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub workspace: Workspace,
    /// Named profile in force, carried into every policy request the chain makes.
    pub profile: String,
    /// True when nothing can ask a human.
    pub unattended: bool,
    /// The grant the profile computed, before any plugin manifest narrows it.
    pub permissions: PermissionSet,
    /// `[sandbox] provider`, carried to every dispatch as the default confinement.
    pub sandbox_provider: Option<String>,
    /// Where an approval goes. `None` means nothing can ask a human, and step 6 refuses
    /// rather than passing quietly — see [`crate::approval`].
    pub approval_sink: Option<Arc<dyn ApprovalSink>>,
    /// Tripped by Ctrl-C. The deadline gets its own, and tools see both.
    pub cancel: CancellationToken,
    pub tool_timeout_ms: Option<u64>,
    pub max_output_bytes: u64,
    /// How long a cancelled tool has to come back before it is abandoned.
    pub cancel_grace: Duration,
}

impl RunConfig {
    /// A configuration with the ordinary defaults.
    #[must_use]
    pub fn new(agent: AgentSpec, session_id: SessionId, workspace: Workspace) -> Self {
        Self {
            agent,
            session_id,
            run_id: RunId::new(),
            workspace,
            profile: "developer".to_string(),
            unattended: false,
            permissions: PermissionSet::empty(),
            sandbox_provider: None,
            approval_sink: None,
            cancel: CancellationToken::new(),
            tool_timeout_ms: Some(120_000),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            cancel_grace: DEFAULT_CANCEL_GRACE,
        }
    }
}

/// Drives model turns against a session.
#[derive(Clone, Debug)]
pub struct AgentLoop {
    registry: Registry,
    session: Arc<dyn SessionStore>,
    bus: Arc<dyn EventBus>,
    assembler: ContextAssembler,
    dispatcher: ToolDispatcher,
    retry: Arc<dyn RetryPolicy>,
    jitter: Arc<dyn Jitter>,
}

impl AgentLoop {
    #[must_use]
    pub fn new(
        registry: Registry,
        session: Arc<dyn SessionStore>,
        bus: Arc<dyn EventBus>,
        assembler: ContextAssembler,
        retry: Arc<dyn RetryPolicy>,
        jitter: Arc<dyn Jitter>,
    ) -> Self {
        let dispatcher = ToolDispatcher::new(registry.clone(), bus.clone());
        Self {
            registry,
            session,
            bus,
            assembler,
            dispatcher,
            retry,
            jitter,
        }
    }

    /// Run turns until the model is done or a limit stops it.
    ///
    /// `state` is the conversation so far, already repaired by
    /// [`crate::session_recovery::close_interrupted`] if this is a resume. `input` is the
    /// new user message, if there is one.
    ///
    /// # Errors
    /// Only for failures that make continuing dishonest: an unresolvable model, or a
    /// session log that cannot be written. A run that *ends badly* returns `Ok` with a
    /// [`StopReason`] saying why.
    pub async fn run(
        &self,
        cfg: RunConfig,
        mut state: SessionState,
        input: Option<Message>,
    ) -> rivet_core::Result<RunSummary> {
        let started = Instant::now();
        let limits = cfg.agent.limits;
        let model = self.registry.model(&cfg.agent.model).await.ok_or_else(|| {
            Error::not_found(format!(
                "no model plugin is registered for `{}`",
                cfg.agent.model
            ))
        })?;

        let writer = SessionWriter::at_state(self.session.clone(), cfg.session_id, &state);
        writer
            .append(SessionEvent::RunStarted {
                run_id: cfg.run_id,
                agent_id: cfg.agent.id,
                model: cfg.agent.model.clone(),
                job_id: None,
            })
            .await?;
        if let Some(message) = input {
            let stored = writer
                .append(SessionEvent::UserMessage {
                    message: message.clone(),
                })
                .await?;
            state.apply(&stored);
        }
        self.publish(
            &cfg,
            AgentEvent::RunStarted {
                agent_id: cfg.agent.id,
                model: cfg.agent.model.clone(),
            },
        );

        let clock = Watchdog::start(&cfg, limits.max_duration_ms);
        let mut tally = Tally::default();
        // Seeded from the log's own projection, which is the whole of "a remembered
        // approval survives a resume": the grants a person made in an earlier run are
        // exactly the ones replay put here.
        let approvals = Approvals::new(cfg.approval_sink.clone())
            .with_remembered(state.remembered_approvals().to_vec());

        let stop = self
            .turns(
                &cfg, &model, &writer, &clock, &approvals, &mut state, &mut tally,
            )
            .await;
        // Whatever happened, the watchdog stops here.
        clock.stop();

        let stop = match stop {
            Ok(stop) => stop,
            Err(error) => {
                // A log we cannot write is a run we must not keep running: the next resume
                // would be reasoning from a record that stopped being true.
                tracing::error!(error = %error, "run failed");
                return Err(error);
            }
        };

        writer
            .append(SessionEvent::RunCompleted {
                run_id: cfg.run_id,
                stop: stop.clone(),
                turns: tally.turns,
            })
            .await?;
        self.publish(
            &cfg,
            AgentEvent::RunCompleted {
                turns: tally.turns,
                stop: stop.clone(),
            },
        );

        Ok(RunSummary {
            run_id: cfg.run_id,
            session_id: cfg.session_id,
            agent_id: cfg.agent.id,
            job_id: None,
            stop,
            turns: tally.turns,
            usage: tally.usage,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            tool_calls: tally.tool_calls,
            tool_errors: tally.tool_errors,
        })
    }

    /// The turn loop proper.
    #[allow(clippy::too_many_arguments)] // Each one is a distinct thing a turn needs.
    async fn turns(
        &self,
        cfg: &RunConfig,
        model: &Arc<dyn Model>,
        writer: &SessionWriter,
        clock: &Watchdog,
        approvals: &Approvals,
        state: &mut SessionState,
        tally: &mut Tally,
    ) -> rivet_core::Result<StopReason> {
        let limits = cfg.agent.limits;
        loop {
            if let Some(stop) = clock.stopped_by(tally, limits) {
                return Ok(stop);
            }

            tally.turns += 1;
            self.publish(cfg, AgentEvent::TurnStarted { turn: tally.turns });

            let request = match self.build_request(cfg, model, state, tally.turns).await? {
                Ok(request) => request,
                Err(stop) => return Ok(stop),
            };

            let completed = match self.request(cfg, model, writer, clock, request).await? {
                Ok(completed) => completed,
                Err(stop) => return Ok(stop),
            };

            let stored = writer
                .append(SessionEvent::AssistantMessage {
                    run_id: cfg.run_id,
                    message: completed.message.clone(),
                    stop_reason: completed.stop_reason,
                    usage: completed.usage,
                })
                .await?;
            state.apply(&stored);
            tally.add_usage(completed.usage);
            self.publish(
                cfg,
                AgentEvent::RequestCompleted {
                    usage: completed.usage,
                    stop_reason: completed.stop_reason,
                    latency_ms: completed.latency_ms,
                },
            );

            match completed.stop_reason {
                // Indistinguishable from EndTurn on an OpenAI-compatible wire, and both
                // end the run, so nothing downstream depends on telling them apart.
                rivet_core::model::StopReason::EndTurn
                | rivet_core::model::StopReason::StopSequence => {
                    self.publish(cfg, AgentEvent::TurnCompleted { turn: tally.turns });
                    return Ok(StopReason::EndTurn);
                }
                // The partial message is already recorded. Reporting success here would
                // hand the caller a truncated answer as a finished one.
                rivet_core::model::StopReason::MaxTokens => {
                    return Ok(StopReason::Error {
                        message: "the model stopped at its output token limit; \
                                  the answer is incomplete"
                            .to_string(),
                    });
                }
                rivet_core::model::StopReason::Refusal => {
                    return Ok(StopReason::Error {
                        message: "the model refused to answer".to_string(),
                    });
                }
                rivet_core::model::StopReason::ToolUse => {
                    let calls: Vec<ToolCall> = completed
                        .message
                        .tool_calls()
                        .into_iter()
                        .cloned()
                        .collect();
                    if let Some(stop) = self
                        .run_tools(cfg, writer, clock, approvals, state, tally, calls)
                        .await?
                    {
                        return Ok(stop);
                    }
                    self.publish(cfg, AgentEvent::TurnCompleted { turn: tally.turns });
                }
            }
        }
    }

    /// Assemble context and turn it into a request, or report why it cannot be made.
    async fn build_request(
        &self,
        cfg: &RunConfig,
        model: &Arc<dyn Model>,
        state: &SessionState,
        turn: u32,
    ) -> rivet_core::Result<Result<ModelRequest, StopReason>> {
        let limits = cfg.agent.limits;

        // Tools first, because their schemas are part of every request and therefore part
        // of the budget the assembler has to pack into. Assembling against the *whole*
        // window and only then adding schemas is how a session lands a few hundred tokens
        // under the limit, gets told it fits, and is refused by the `count_tokens`
        // re-check below -- deterministically, so `resume` would do it again.
        let mut tools = Vec::new();
        for spec in self.registry.tool_specs().await {
            // Pipeline step 2 applied ahead of time: a tool outside the agent's scope is
            // never offered, so the model does not spend a turn being refused.
            if !cfg.agent.allows_tool(&spec.name) {
                continue;
            }
            tools.push((*spec).clone());
        }
        let tool_tokens = estimate_tool_tokens(&tools);

        // A window that the tool schemas alone fill leaves nothing to say, and trimming
        // history cannot recover it -- the tools are not negotiable.
        let Some(budget_tokens) = limits.max_context_tokens.checked_sub(tool_tokens) else {
            tracing::warn!(
                tool_tokens,
                budget = limits.max_context_tokens,
                "the tool schemas alone exceed the context budget"
            );
            return Ok(Err(StopReason::LimitReached {
                limit: LimitKind::ContextSize,
            }));
        };

        let context_request = ContextRequest {
            session_id: cfg.session_id,
            agent_id: cfg.agent.id,
            run_id: cfg.run_id,
            job_id: None,
            workspace: cfg.workspace.clone(),
            turn: turn.saturating_sub(1),
            budget_tokens,
        };
        // A checkpoint projects its summary into the first message; losing it to a trim
        // would be amnesia about everything the checkpoint folded away.
        let pinned = usize::from(state.checkpoint_summary.is_some());

        let assembled = match self
            .assembler
            .assemble(&context_request, &state.messages, pinned)
            .await
        {
            Ok(assembled) => assembled,
            Err(overflow) => {
                tracing::warn!(%overflow, "context does not fit");
                return Ok(Err(StopReason::LimitReached {
                    limit: LimitKind::ContextSize,
                }));
            }
        };

        let request = ModelRequest {
            model: cfg.agent.model.clone(),
            system: (!assembled.system.is_empty()).then_some(assembled.system),
            messages: assembled.messages,
            tools,
            tool_choice: ToolChoice::Auto,
            params: ModelParams::default(),
        };

        // The assembler budgeted with its own estimate; ask the model's counter too. A
        // provider with a real tokenizer disagrees, and the request is about to be sent.
        let counted = model.count_tokens(&request).await?;
        if counted > u64::from(limits.max_context_tokens) {
            tracing::warn!(
                counted,
                budget = limits.max_context_tokens,
                "the model's own token count exceeds the context budget"
            );
            return Ok(Err(StopReason::LimitReached {
                limit: LimitKind::ContextSize,
            }));
        }

        dump_request(&request);
        Ok(Ok(request))
    }

    /// One model round trip, with retries.
    async fn request(
        &self,
        cfg: &RunConfig,
        model: &Arc<dyn Model>,
        writer: &SessionWriter,
        clock: &Watchdog,
        request: ModelRequest,
    ) -> rivet_core::Result<Result<Completed, StopReason>> {
        let digest = request_digest(&request)?;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            // Recorded before the request goes out, and once per attempt, so a crash
            // mid-request shows up on replay as a request with no completion.
            writer
                .append(SessionEvent::ModelRequested {
                    run_id: cfg.run_id,
                    model: cfg.agent.model.clone(),
                    request_digest: digest.clone(),
                })
                .await?;
            self.publish(
                cfg,
                AgentEvent::RequestStarted {
                    model: cfg.agent.model.clone(),
                    input_tokens_estimate: model.count_tokens(&request).await.unwrap_or(0),
                },
            );

            let started = Instant::now();
            match self.stream(cfg, model, clock, request.clone()).await {
                Ok(mut completed) => {
                    completed.latency_ms =
                        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    return Ok(Ok(completed));
                }
                Err(error) if error.is_cancelled() => {
                    return Ok(Err(clock.cancellation_reason()));
                }
                Err(error) => {
                    let decision = self.retry.should_retry(&error, attempt);
                    let will_retry = matches!(decision, RetryDecision::RetryAfter { .. });
                    self.publish(
                        cfg,
                        AgentEvent::RequestFailed {
                            error: error.to_string(),
                            will_retry,
                            attempt,
                        },
                    );
                    match decision {
                        RetryDecision::RetryAfter { delay_ms } => {
                            let delay = self.jitter.apply(delay_ms);
                            tracing::warn!(
                                attempt,
                                delay_ms = delay,
                                error = %error,
                                "model request failed; retrying"
                            );
                            // A retry is not a turn: a turn is one request plus the tool
                            // calls it caused, and a retried request is still that one.
                            tokio::select! {
                                () = tokio::time::sleep(Duration::from_millis(delay)) => {}
                                () = clock.effective.cancelled() => {
                                    return Ok(Err(clock.cancellation_reason()));
                                }
                            }
                        }
                        RetryDecision::Stop => {
                            return Ok(Err(StopReason::Error {
                                message: format!("the model request failed: {error}"),
                            }));
                        }
                    }
                }
            }
        }
    }

    /// Consume one stream to its `Done`, publishing deltas as they arrive.
    async fn stream(
        &self,
        cfg: &RunConfig,
        model: &Arc<dyn Model>,
        clock: &Watchdog,
        request: ModelRequest,
    ) -> rivet_core::Result<Completed> {
        use futures_util::StreamExt;

        let mut stream = model.stream(request).await?;
        loop {
            tokio::select! {
                // Dropping the stream is what aborts the in-flight HTTP request; the
                // Model contract requires adapters to honor that.
                () = clock.effective.cancelled() => {
                    return Err(Error::cancelled("the run was cancelled mid-request"));
                }
                item = stream.next() => match item {
                    Some(Ok(StreamEvent::TextDelta { text, .. })) => {
                        // Bus only. The session stores the assembled message, so a replay
                        // never has to re-derive it from deltas.
                        self.publish(cfg, AgentEvent::TextDelta { text });
                    }
                    Some(Ok(StreamEvent::Done { message, stop_reason, usage })) => {
                        return Ok(Completed {
                            message,
                            stop_reason,
                            usage,
                            latency_ms: 0,
                        });
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error),
                    None => {
                        return Err(Error::transient(
                            rivet_core::error::Capability::Model,
                            "the model stream ended without a completion",
                        ));
                    }
                },
            }
        }
    }

    /// Run a turn's tool calls in order, closing whatever is left if the turn ends early.
    ///
    /// Returns `Some(stop)` when the run is over.
    #[allow(clippy::too_many_arguments)] // Each one is a distinct thing a turn needs.
    async fn run_tools(
        &self,
        cfg: &RunConfig,
        writer: &SessionWriter,
        clock: &Watchdog,
        approvals: &Approvals,
        state: &mut SessionState,
        tally: &mut Tally,
        calls: Vec<ToolCall>,
    ) -> rivet_core::Result<Option<StopReason>> {
        let limits = cfg.agent.limits;
        let ctx = DispatchCtx {
            agent: Arc::new(cfg.agent.clone()),
            session_id: cfg.session_id,
            agent_id: cfg.agent.id,
            run_id: cfg.run_id,
            workspace: cfg.workspace.clone(),
            permissions: cfg.permissions.clone(),
            profile: cfg.profile.clone(),
            unattended: cfg.unattended,
            timeout_ms: cfg.tool_timeout_ms,
            max_output_bytes: cfg.max_output_bytes,
            sandbox_provider: cfg.sandbox_provider.clone(),
            approvals: approvals.clone(),
            cancel: clock.effective.clone(),
            grace: cfg.cancel_grace,
        };

        // Sequential, not parallel: `read_only` is a hint a tool author writes about their
        // own tool, and the design does not treat a hint as a safety argument.
        let mut index = 0;
        let mut stop = None;
        while index < calls.len() {
            if clock.effective.is_cancelled() {
                stop = Some(clock.cancellation_reason());
                break;
            }

            let disposition = self
                .dispatcher
                .dispatch(&ctx, writer, calls[index].clone())
                .await?;
            index += 1;
            tally.tool_calls += 1;

            // Mirror what `SessionState::apply` will project, so the in-memory
            // conversation and a replay of the log stay identical.
            state.messages.push(Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    call_id: calls[index - 1].id,
                    content: disposition.content(),
                    is_error: disposition.is_error(),
                }],
            });

            if disposition.is_error() {
                tally.tool_errors += 1;
            }
            if disposition.counts_as_error() {
                tally.consecutive_errors += 1;
            } else {
                tally.consecutive_errors = 0;
            }

            // Checked here rather than at the end of the batch: once the run is over there
            // is no reason to keep running the tools it asked for.
            if tally.consecutive_errors > 0
                && tally.consecutive_errors >= limits.max_consecutive_tool_errors
            {
                stop = Some(StopReason::LimitReached {
                    limit: LimitKind::ConsecutiveToolErrors,
                });
                break;
            }
        }

        if let Some(stop) = stop {
            // Every unanswered call gets an answer, or this session cannot be resumed --
            // the price of doing cancellation right would be breaking `rivet resume`.
            self.close_remaining(cfg, writer, state, &calls[index..])
                .await?;
            return Ok(Some(stop));
        }
        Ok(None)
    }

    /// Record a terminating result for calls that never reached the dispatcher.
    async fn close_remaining(
        &self,
        cfg: &RunConfig,
        writer: &SessionWriter,
        state: &mut SessionState,
        remaining: &[ToolCall],
    ) -> rivet_core::Result<()> {
        for call in remaining {
            // A distinct wording from an interrupted call: this one provably had no
            // effect, so the model can retry it safely.
            let result = ToolResult::error(NOT_STARTED_TOOL_RESULT);
            writer
                .append(SessionEvent::ToolCompleted {
                    run_id: cfg.run_id,
                    call_id: call.id,
                    result: result.clone(),
                    duration_ms: 0,
                })
                .await?;
            state.messages.push(Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    call_id: call.id,
                    content: result.content,
                    is_error: true,
                }],
            });
        }
        Ok(())
    }

    fn publish(&self, cfg: &RunConfig, event: AgentEvent) {
        self.bus
            .publish(EventEnvelope::new(Event::Agent(event)).for_run(cfg.session_id, cfg.run_id));
    }
}

/// One completed model round trip.
#[derive(Debug)]
struct Completed {
    message: Message,
    stop_reason: rivet_core::model::StopReason,
    usage: Usage,
    latency_ms: u64,
}

/// Running counts for a run.
#[derive(Debug, Default)]
struct Tally {
    turns: u32,
    usage: Usage,
    tool_calls: u32,
    tool_errors: u32,
    consecutive_errors: u32,
}

impl Tally {
    fn add_usage(&mut self, usage: Usage) {
        self.usage.input_tokens += usage.input_tokens;
        self.usage.output_tokens += usage.output_tokens;
        self.usage.cache_read_tokens += usage.cache_read_tokens;
        self.usage.cache_write_tokens += usage.cache_write_tokens;
    }
}

/// The run deadline, and the token everything downstream watches.
///
/// A deadline checked only at a turn boundary is not a deadline: one tool call or one
/// stream can run past it on its own. So a watcher task trips the token instead, and the
/// loop's existing observation points — the stream's `select!`, the tool's
/// `is_cancelled()` — do the stopping.
#[derive(Debug)]
struct Watchdog {
    effective: CancellationToken,
    deadline_fired: Arc<AtomicBool>,
    finished: CancellationToken,
    started: Instant,
    handle: tokio::task::JoinHandle<()>,
}

impl Watchdog {
    fn start(cfg: &RunConfig, max_duration_ms: u64) -> Self {
        // A **child** of the user's token, not a separate one a task bridges across.
        // Cancellation propagates down a token tree inside `cancel()` itself, so Ctrl-C is
        // visible to the loop the instant it happens. Bridging the two with a spawned task
        // meant that under load the task might not be scheduled for a whole turn -- the
        // run would finish normally after the user had already asked it to stop, and the
        // five-second budget would be measured from a moment nobody observed.
        //
        // The relation only runs one way: cancelling this token does not cancel the user's,
        // which is what `docs/security.md` requires of a tool timeout.
        let effective = cfg.cancel.child_token();
        let deadline_fired = Arc::new(AtomicBool::new(false));
        let finished = CancellationToken::new();

        // The task now only owns the deadline.
        let handle = tokio::spawn({
            let effective = effective.clone();
            let fired = deadline_fired.clone();
            let finished = finished.clone();
            async move {
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(max_duration_ms)) => {
                        fired.store(true, Ordering::SeqCst);
                        effective.cancel();
                    }
                    () = finished.cancelled() => {}
                }
            }
        });

        Self {
            effective,
            deadline_fired,
            finished,
            started: Instant::now(),
            handle,
        }
    }

    fn stop(&self) {
        self.finished.cancel();
        self.handle.abort();
    }

    /// Which ending a tripped token means. The user stopping a run and a run running out
    /// of time are different facts, and the log should say which.
    fn cancellation_reason(&self) -> StopReason {
        if self.deadline_fired.load(Ordering::SeqCst) {
            StopReason::LimitReached {
                limit: LimitKind::Duration,
            }
        } else {
            StopReason::Cancelled
        }
    }

    /// The turn-boundary check for all five limits.
    fn stopped_by(&self, tally: &Tally, limits: RunLimits) -> Option<StopReason> {
        if self.effective.is_cancelled() {
            return Some(self.cancellation_reason());
        }
        if tally.turns >= limits.max_turns {
            return Some(StopReason::LimitReached {
                limit: LimitKind::Turns,
            });
        }
        if u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
            >= limits.max_duration_ms
        {
            return Some(StopReason::LimitReached {
                limit: LimitKind::Duration,
            });
        }
        if tally.usage.total() >= limits.max_total_tokens {
            return Some(StopReason::LimitReached {
                limit: LimitKind::Tokens,
            });
        }
        // `max_context_tokens` is checked where it can actually be measured: in
        // `build_request`, against the assembled request.
        None
    }
}

/// What the tool schemas will cost on the wire.
///
/// Deliberately the same arithmetic as [`Model::count_tokens`]'s default body. A provider
/// with a real tokenizer will disagree, which is why the re-check after assembly stays --
/// this only stops the two *estimates* from contradicting each other.
fn estimate_tool_tokens(tools: &[ToolSpec]) -> u32 {
    let mut total: u32 = 0;
    for tool in tools {
        total = total.saturating_add(estimate_tokens(&tool.description));
        total = total.saturating_add(estimate_tokens(&tool.input_schema.to_string()));
    }
    total
}

/// Write an assembled request to [`DUMP_REQUESTS_ENV`], if an operator asked for it.
fn dump_request(request: &ModelRequest) {
    let Ok(dir) = std::env::var(DUMP_REQUESTS_ENV) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = std::path::Path::new(&dir).join(format!(
        "request-{}.json",
        rivet_core::Timestamp::now().as_millis()
    ));
    if let Ok(json) = serde_json::to_vec_pretty(request) {
        let _ = std::fs::write(path, json);
    }
}
