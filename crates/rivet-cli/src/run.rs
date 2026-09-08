//! Starting and resuming runs.

use std::path::Path;
use std::sync::Arc;

use rivet_core::agent::{AgentSpec, RunSummary, StopReason};
use rivet_core::error::Error;
use rivet_core::event::EventSubscriber;
use rivet_core::id::SessionId;
use rivet_core::model::Message;
use rivet_core::retry::ExponentialBackoff;
use rivet_core::session::{SessionEvent, SessionState, SessionStore};
use rivet_runtime::agent_loop::{AgentLoop, RunConfig};
use rivet_runtime::context::ContextAssembler;
use rivet_runtime::jitter::FullJitter;
use rivet_runtime::{BroadcastBus, Drained};
use rivet_session::JsonlSessionStore;
use rivet_tui::{Intent, Tui};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::catalog::{self, Host};
use crate::config::Config;
use crate::render::{self, Observers};

pub use crate::render::Output;

/// How long the host waits for its own renderer to finish the stream.
///
/// Invented, and safe to invent: unlike the missing `Plugin::load` deadline
/// (`docs/architecture.md` §11-15), this is a budget the host puts on *its own output*,
/// not on a plugin's contract. The cost of getting it wrong is one warning line saying the
/// tail was cut — not a `FAILED` record for something that was merely slow.
const DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// Start a new session and run one prompt.
///
/// # Errors
/// Configuration problems, plugin load failures, and session-store failures.
pub async fn start(
    config: &Config,
    prompt: &str,
    output: Output,
) -> rivet_core::Result<RunSummary> {
    // Before a session exists: an empty session left behind by a missing key is noise in
    // `rivet session list` forever.
    config.check_credentials()?;
    // The bus, the observer and `runtime.started` all come before `catalog::load`, so the
    // whole plugin lifecycle happens with somebody listening. Before Phase 3 the observer
    // went on afterwards, so it happened in an empty room and `--jsonl` carried no
    // `plugin.*` line at all.
    let mut watching = observe(output)?;
    let result = started(config, prompt, output, &mut watching).await;
    // Every path out of the run, the failing ones included. See [`Watching::finish`].
    watching.finish().await;
    result
}

/// [`start`] with the observers already attached, so its `?`s have somewhere to land.
async fn started(
    config: &Config,
    prompt: &str,
    output: Output,
    watching: &mut Watching,
) -> rivet_core::Result<RunSummary> {
    let mut host = catalog::load(config, watching.bus.clone()).await?;
    let store = Arc::new(JsonlSessionStore::new(&config.sessions_dir));

    let session_id = SessionId::new();
    store
        .create(
            session_id,
            SessionEvent::Created {
                workspace_root: config.workspace.root().display().to_string(),
                parent: None,
            },
        )
        .await?;
    eprintln!("session {session_id}");

    let state = SessionState::replay(&store.read(session_id, 1, 1_000).await?);
    drive(
        config,
        &mut host,
        store,
        session_id,
        state,
        Some(Message::user(prompt)),
        output,
        watching,
    )
    .await
}

/// Everything `observe` sets up, before a single plugin is constructed.
#[derive(Debug)]
struct Watching {
    bus: BroadcastBus,
    observers: Observers,
    /// The screen, when `--tui` is on. It is both the subscriber and the thing the render
    /// loop draws, so the two halves have to be the same value.
    tui: Option<Arc<Tui>>,
    intents: Option<mpsc::Receiver<Intent>>,
}

impl Watching {
    /// Deliver everything published so far, then stop the host's own consumers.
    ///
    /// Called on the way out of a run *and* on every path that never reaches one. The
    /// second is the one worth spelling out: `catalog::load` publishes `plugin.discovered`,
    /// `plugin.load.failed` and the rollback that follows it, and a `?` that merely dropped
    /// `Watching` would hit [`rivet_runtime::Observer`]'s `Drop`, which **aborts** the pump
    /// rather than draining it. So `--jsonl` lost its last and most useful lines at exactly
    /// the moment an operator is reading the stream for them.
    ///
    /// Idempotent: [`drive`] takes the observers for its own drain, and what is left behind
    /// drains to `Complete` at once.
    async fn finish(&mut self) {
        let observers = std::mem::take(&mut self.observers);
        if let Drained::Truncated { budget_ms } = observers.drain_within(DRAIN_BUDGET).await {
            eprintln!(
                "rivet: the event stream was cut off after {budget_ms}ms; \
                 its last lines are missing"
            );
        }
    }
}

/// Make the bus, attach the host's renderer, and announce the runtime — in that order.
///
/// The order *is* the point. An observer attached after `catalog::load` misses
/// `runtime.started` and every `plugin.*` event, which is most of what an operator reads
/// the stream for.
///
/// # Errors
/// `--tui` with stdout on a pipe.
fn observe(output: Output) -> rivet_core::Result<Watching> {
    if output == Output::Tui && !rivet_tui::is_a_terminal() {
        // Before raw mode, not after: raw mode on a pipe leaves no terminal to put back.
        return Err(Error::invalid_argument(
            "`--tui` needs a terminal; use `--jsonl` when stdout is a pipe",
        ));
    }

    let bus = BroadcastBus::new();
    let (tui, intents) = match output {
        Output::Tui => {
            let (tui, intents) = Tui::new();
            (Some(Arc::new(tui)), Some(intents))
        }
        Output::Human | Output::Jsonl => (None, None),
    };
    let observers = render::attach(
        &bus,
        output,
        tui.clone().map(|tui| tui as Arc<dyn EventSubscriber>),
    );
    // `runtime.started` is the host's to publish, and `rivet run`/`rivet resume` are the
    // only commands that start a runtime -- `doctor` diagnoses one, which is not the same
    // thing and would blur what the topic means.
    rivet_runtime::lifecycle::started(&bus, env!("CARGO_PKG_VERSION"));
    Ok(Watching {
        bus,
        observers,
        tui,
        intents,
    })
}

/// Continue an interrupted session.
///
/// # Errors
/// As [`start`], plus [`rivet_core::error::ErrorKind::Storage`] when the log cannot be
/// repaired by appending, and `InvalidArgument` when the session was created against a
/// different workspace.
pub async fn resume(
    config: &Config,
    session: &str,
    output: Output,
) -> rivet_core::Result<Option<RunSummary>> {
    config.check_credentials()?;
    let session_id: SessionId = session.parse().map_err(|e| {
        Error::invalid_argument(format!("`{session}` is not a session id")).with_cause(e)
    })?;

    let store = Arc::new(JsonlSessionStore::new(&config.sessions_dir));
    let events = rivet_runtime::session_recovery::read_all(store.as_ref(), session_id).await?;
    check_workspace(config, &events)?;

    // Close whatever the interruption left open, before anything else looks at the
    // conversation. Doing this in the runtime rather than here means every host that
    // resumes a session gets the same repair.
    let state =
        rivet_runtime::session_recovery::close_interrupted(store.as_ref(), session_id).await?;

    match rivet_runtime::session_recovery::resume_plan(&state) {
        rivet_runtime::session_recovery::ResumePlan::Refuse(reason) => {
            eprintln!("{reason}");
            Ok(None)
        }
        rivet_runtime::session_recovery::ResumePlan::Continue => {
            let mut watching = observe(output)?;
            let result = resumed(config, store, session_id, state, output, &mut watching).await;
            watching.finish().await;
            Ok(Some(result?))
        }
    }
}

/// [`resume`] with the observers already attached, so its `?`s have somewhere to land.
async fn resumed(
    config: &Config,
    store: Arc<JsonlSessionStore>,
    session_id: SessionId,
    state: SessionState,
    output: Output,
    watching: &mut Watching,
) -> rivet_core::Result<RunSummary> {
    let mut host = catalog::load(config, watching.bus.clone()).await?;
    drive(
        config, &mut host, store, session_id, state, None, output, watching,
    )
    .await
}

/// Refuse to replay a session that was recorded somewhere else.
///
/// `session.created` records the workspace root precisely so this can be checked. Silently
/// continuing would run another repository's tool calls against this one.
fn check_workspace(
    config: &Config,
    events: &[rivet_core::session::StoredEvent],
) -> rivet_core::Result<()> {
    let Some(recorded) = events.iter().find_map(|e| match &e.event {
        SessionEvent::Created { workspace_root, .. } => Some(workspace_root.clone()),
        _ => None,
    }) else {
        return Ok(());
    };
    let current = config.workspace.root();
    if Path::new(&recorded) == current {
        return Ok(());
    }
    Err(Error::invalid_argument(format!(
        "this session was created in `{recorded}` but the workspace here is `{}`. \
         Resuming would run its tool calls against a different tree; \
         run `rivet resume` from the original workspace instead.",
        current.display()
    )))
}

/// Wire up the signal handler and the loop, then run.
///
/// The renderers are already attached — [`observe`] did that before any plugin existed —
/// so what happens here is the run itself, the shutdown announcement, and finishing the
/// stream.
#[allow(clippy::too_many_arguments)] // Every one is a distinct thing the run needs.
async fn drive(
    config: &Config,
    host: &mut Host,
    store: Arc<JsonlSessionStore>,
    session_id: SessionId,
    state: SessionState,
    input: Option<Message>,
    output: Output,
    watching: &mut Watching,
) -> rivet_core::Result<RunSummary> {
    let agent = agent_spec(config);
    let providers = host.registry.context_providers().await;
    // Phase 1's agent never names providers, so this takes the "all of them" branch. The
    // selecting branch exists so a named agent works the moment there is a way to pick one.
    let assembler = ContextAssembler::for_agent(providers, &agent.context_providers)?;
    let cancel = CancellationToken::new();
    let signals = crate::signals::install(cancel.clone());

    // Who can answer an approval, and therefore whether this run is attended at all.
    //
    // `Config::unattended` records what the *operator* said — `--headless`, or the `ci`
    // profile. Whether a human can actually be reached is a different question, and only
    // this layer knows the answer: it depends on the output mode and on whether stdin is a
    // terminal. A run piped from `/dev/null` without `--headless` has nobody to ask, and a
    // prompt there would hang on a read that never returns.
    let sink: Option<Arc<dyn rivet_core::policy::ApprovalSink>> = match (output, &watching.tui) {
        (Output::Tui, Some(tui)) => Some(tui.clone()),
        (Output::Tui, None) => None,
        (Output::Human | Output::Jsonl, _) => crate::render::approve::PromptApprover::for_stdin()
            .map(|approver| Arc::new(approver) as Arc<dyn rivet_core::policy::ApprovalSink>),
    };

    let mut cfg = RunConfig::new(agent, session_id, config.workspace.clone());
    cfg.profile = config.profile.name().to_string();
    cfg.unattended = unattended(config.unattended, sink.is_some());
    cfg.permissions = config.profile.permissions();
    cfg.sandbox_provider = Some(config.sandbox_provider.clone());
    cfg.approval_sink = sink;
    cfg.cancel = cancel.clone();

    let agent_loop = AgentLoop::new(
        host.registry.clone(),
        store,
        host.registry.events(),
        assembler,
        Arc::new(ExponentialBackoff::default()),
        Arc::new(FullJitter::for_run(cfg.run_id)),
    );

    // The screen, if there is one. `TerminalGuard` owns raw mode for as long as it lives,
    // so the run happens inside its scope and the summary is printed after `restore`.
    let mut screen = match (&watching.tui, watching.intents.take()) {
        (Some(tui), Some(intents)) => Some(Screen::start(tui, intents, &cancel)?),
        _ => None,
    };

    let summary = agent_loop.run(cfg, state, input).await;

    signals.abort();
    host.shutdown();
    // `runtime.shutting_down` before the unloads it explains, so a plugin's own subscriber
    // sees the reason it is about to be detached. The `plugin.unloaded` lines that follow
    // are the ones it necessarily misses -- which is what DoD 5 asks for.
    rivet_runtime::lifecycle::shutting_down(&host.bus, "run finished");
    // `Plugin::unload` finally has a caller on the normal path: a plugin that started
    // something of its own gets told the run is over, rather than being left to process
    // exit.
    host.loader.unload_all().await;

    // Everything is published. Hand it over -- rather than sleeping 20 ms and aborting,
    // which made the tail of the stream a matter of scheduler luck. Before the summary, so
    // the summary is the last thing on the screen rather than a line the stream runs over.
    watching.finish().await;

    // The screen comes down *after* the drain, not before it. Everything above this line
    // publishes -- `runtime.shutting_down` most of all -- and a screen stopped first folds
    // all of that into a state nobody ever draws, which is how the status bar's "shutting
    // down" segment came to be rendered, asserted on, and never once seen. `render_loop`
    // draws one final frame on its way out, so by here the ending has actually been shown.
    if let Some(screen) = &mut screen {
        screen.stop().await;
    }

    let summary = summary?;
    // The terminal is back, and with it the answer. `--tui` attaches the screen as the run's
    // *only* subscriber -- `render::attach` returns one observer, and for `Tui` that is it,
    // so no `HumanRenderer` is streaming underneath -- and every delta the model produced
    // lives on the alternate screen that has just been discarded. Without this the reply
    // flashes away at the moment it completes and `rivet session show` is the only way back
    // to it. `Human` streamed it as it arrived and `Jsonl` carries it as events; this is the
    // one mode with nowhere else to put it.
    if let Some(tui) = &watching.tui {
        answer(&tui.snapshot().run);
    }
    // Not `Jsonl`: that stream is for a machine and a prose footer would be a parse error.
    // `Tui` prints it like `Human` does -- the terminal has been restored by now, so it
    // lands on the real screen, which is what the alternate screen going away is for.
    if output != Output::Jsonl {
        report(&summary);
    }
    Ok(summary)
}

/// Whether this run has nobody to ask.
///
/// Two independent reasons, and they are both real. `configured` is what the *operator*
/// said — `--headless`, or the `ci` profile. `has_sink` is whether a human can actually be
/// reached, which only this layer knows: it depends on the output mode and on whether stdin
/// is a terminal. A run piped from `/dev/null` without `--headless` has nobody to ask, and
/// a prompt there would park on a read that never returns — which is exactly the hang
/// `--headless` exists to prevent, arriving through a door nobody marked.
fn unattended(configured: bool, has_sink: bool) -> bool {
    configured || !has_sink
}

/// Print what the TUI was showing, once the alternate screen is gone.
///
/// To stdout, where `HumanRenderer` streams the same text: the answer is the output, and
/// `--tui` refuses a pipe, so this is a terminal either way.
///
/// The panel keeps a bounded buffer, so a long enough run has already lost its opening.
/// Saying so beats printing a reply that silently begins in the middle -- and it is the
/// same sentence, and the same count, the panel itself was showing.
fn answer(run: &rivet_tui::RunView) {
    if run.text.is_empty() {
        return;
    }
    if run.text_dropped > 0 {
        eprintln!(
            "  · the first {} character(s) scrolled out of the panel; \
             `rivet session show` has the whole reply",
            run.text_dropped
        );
    }
    println!("{}", run.text);
}

/// The TUI while it is on screen: raw mode, the render loop, and the intent pump.
///
/// A value rather than three locals so `drive` has one thing to stop, and so the terminal
/// is put back on every path out — including the error one. That second half is what
/// [`Screen::drop`] is for: it was asserted in this comment and implemented nowhere, and
/// held only because no `?` happens to sit between [`Screen::start`] and [`Screen::stop`]
/// today.
///
/// The handles are `Option` so both ways out can take them — [`Screen::stop`] needs to
/// `await` one, which it could not do out of a type that also implements `Drop`.
#[derive(Debug)]
struct Screen {
    render: Option<tokio::task::JoinHandle<()>>,
    intents: Option<tokio::task::JoinHandle<()>>,
    /// Ends the render loop. A child of nothing: cancelling the *run* should not
    /// immediately blank the screen, because the run still has its shutdown to do.
    stop: CancellationToken,
}

impl Screen {
    /// Enter raw mode and start drawing.
    ///
    /// # Errors
    /// Whatever the terminal reports on entering raw mode.
    fn start(
        tui: &Arc<Tui>,
        mut intents: mpsc::Receiver<Intent>,
        run_cancel: &CancellationToken,
    ) -> rivet_core::Result<Self> {
        let mut guard = rivet_tui::TerminalGuard::enter().map_err(|e| {
            Error::internal("could not put the terminal into raw mode").with_cause(e)
        })?;
        let stop = CancellationToken::new();

        let render = tokio::spawn({
            let (tui, stop) = (tui.clone(), stop.clone());
            async move {
                let _ = tui.render_loop(guard.terminal(), stop).await;
                // Explicit, not just `Drop`: the summary is printed on the real screen
                // after this, and the forced-exit path below runs no destructors at all.
                guard.restore();
            }
        });

        let intents = tokio::spawn({
            let run_cancel = run_cancel.clone();
            let stop = stop.clone();
            async move {
                let mut asked_to_cancel = false;
                while let Some(intent) = intents.recv().await {
                    match Reaction::to(intent, &mut asked_to_cancel) {
                        Reaction::StopDrawing => {
                            stop.cancel();
                            return;
                        }
                        Reaction::CancelTheRun => run_cancel.cancel(),
                        Reaction::ForceExit => {
                            // `process::exit` runs no destructors, so the terminal is put
                            // back here rather than left to `TerminalGuard::drop`.
                            rivet_tui::terminal::restore();
                            eprintln!(
                                "forced; the session log may end mid-turn and will be \
                                 repaired on resume"
                            );
                            std::process::exit(crate::exit::CANCELLED);
                        }
                    }
                }
            }
        });

        Ok(Self {
            render: Some(render),
            intents: Some(intents),
            stop,
        })
    }

    /// Put the terminal back, then let the tasks go.
    ///
    /// The render task is *awaited* rather than aborted, and the reason is ordering rather
    /// than safety: whatever comes next -- the answer, the summary -- is printed on the real
    /// screen, so `guard.restore()` has to have happened before this returns. Aborting would
    /// restore it too, at some later moment of the runtime's choosing, and the summary would
    /// race the alternate screen going away.
    #[allow(clippy::future_not_send)] // Runs on the same task that built it.
    async fn stop(&mut self) {
        self.stop.cancel();
        if let Some(render) = self.render.take() {
            let _ = render.await;
        }
        if let Some(intents) = self.intents.take() {
            intents.abort();
        }
    }
}

impl Drop for Screen {
    /// The path [`Screen::stop`] never reached.
    ///
    /// A `?` between [`Screen::start`] and the stop, or a panic unwinding through [`drive`].
    /// After a normal `stop` both handles are `None` and this does nothing.
    ///
    /// Aborting is what restores the terminal here: `TerminalGuard` is owned by the render
    /// task, and dropping that task's future drops the guard, whose own `Drop` leaves raw
    /// mode and the alternate screen. There is no `await` in a destructor to order it
    /// against anything, which is exactly why `stop` exists as well as this.
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(render) = self.render.take() {
            render.abort();
        }
        if let Some(intents) = self.intents.take() {
            intents.abort();
        }
    }
}

/// What the host does about one [`Intent`].
///
/// A value rather than three inline branches so the *second* Ctrl-C is testable. In raw
/// mode neither interrupt reaches `signals.rs` — SIGINT arrives as a key — so without this
/// the two-step guarantee Phase 1 made ("the first asks the run to stop and lets it write
/// its log; the second gives up on that") would be quietly half true inside `--tui`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reaction {
    /// Leave the UI; the run carries on to its shutdown.
    StopDrawing,
    /// Ask the run to stop, within its cancellation budget.
    CancelTheRun,
    /// Give up on the budget. The log may end mid-turn and `resume` repairs it.
    ForceExit,
}

impl Reaction {
    /// `asked` carries whether a cancel has already been sent, and is updated here.
    fn to(intent: Intent, asked: &mut bool) -> Self {
        match intent {
            Intent::Quit => Self::StopDrawing,
            Intent::Cancel if *asked => Self::ForceExit,
            Intent::Cancel => {
                *asked = true;
                Self::CancelTheRun
            }
        }
    }
}

/// The agent this configuration describes.
fn agent_spec(config: &Config) -> AgentSpec {
    let mut agent = AgentSpec::new("rivet", config.model.clone());
    agent.instructions.clone_from(&config.instructions);
    agent.limits = config.limits;
    if let Some(scope) = config.profile.tool_scope() {
        agent.tools = scope;
    }
    agent
}

fn report(summary: &RunSummary) {
    eprintln!(
        "  · {} in / {} out tokens, {} tool call(s), {}ms",
        summary.usage.input_tokens,
        summary.usage.output_tokens,
        summary.tool_calls,
        summary.duration_ms
    );
    if let StopReason::Error { message } = &summary.stop {
        eprintln!("  ! {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The second Ctrl-C, which raw mode takes away from `signals.rs`.
    #[test]
    fn a_second_cancel_intent_forces_the_exit() {
        let mut asked = false;
        assert_eq!(
            Reaction::to(Intent::Cancel, &mut asked),
            Reaction::CancelTheRun
        );
        assert!(asked);
        assert_eq!(
            Reaction::to(Intent::Cancel, &mut asked),
            Reaction::ForceExit
        );
    }

    #[test]
    fn a_non_tty_stdin_marks_a_run_unattended() {
        // The half `render::approve` cannot decide for itself: it can say there is no sink,
        // and this is what that costs the run.
        assert!(
            unattended(false, false),
            "nowhere to ask means the approval is refused rather than waited on"
        );
        assert!(unattended(true, true), "`--headless` still wins on its own");
        assert!(
            !unattended(false, true),
            "and an ordinary terminal run asks"
        );
    }

    #[test]
    fn quitting_leaves_the_ui_without_stopping_the_run() {
        // `q` is "stop showing me this", not "abandon the run". The run still gets to
        // publish `runtime.shutting_down` and unload its plugins.
        let mut asked = false;
        assert_eq!(
            Reaction::to(Intent::Quit, &mut asked),
            Reaction::StopDrawing
        );
        assert!(!asked, "quitting is not a cancel");
    }
}
