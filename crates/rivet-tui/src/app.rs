//! The subscriber that is also a screen.
//!
//! [`Tui`] implements [`EventSubscriber`], so the host attaches it with the same
//! `bus.observe` it uses for `--jsonl`, and it holds the state behind a `Mutex`. The
//! rendering runs on its own task: the pump must never wait for a redraw, and a redraw must
//! never wait for an event.
//!
//! Keys leave as [`Intent`]. The TUI does not cancel the run; it says somebody asked to.
//! The host owns the token and decides what "cancel" means — the UI equivalent of
//! `docs/architecture.md` §5.3's rule that subscribers observe and interceptors decide.

use std::fmt;
use std::future::Future;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyEventKind};
use rivet_core::event::{EventEnvelope, EventSubscriber};
use rivet_core::policy::{ApprovalOutcome, ApprovalRequest, ApprovalSink};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::draw::draw;
use crate::state::{AppState, ApprovalView, Panel};

/// How often the screen is redrawn, and how long a key press may wait.
const TICK: Duration = Duration::from_millis(50);

/// What the user asked the host to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Stop showing the UI.
    Quit,
    /// Stop the run. The host maps this onto the same token the signal handler uses.
    Cancel,
}

/// A screen fed by the bus.
pub struct Tui {
    state: Mutex<AppState>,
    intents: mpsc::Sender<Intent>,
    /// Where a key press sends the answer to the approval currently on screen.
    ///
    /// Separate from [`AppState`] on purpose: the state is a value the drawing code clones
    /// and a test builds by hand, and a one-shot sender is neither cloneable nor something a
    /// test should have to construct to draw a modal.
    ///
    /// Two locks, so both are taken together and always `state` first — see
    /// [`Tui::answer_approval`].
    answer: Mutex<Option<oneshot::Sender<ApprovalOutcome>>>,
    /// Cancelled when the screen goes away, and never uncancelled.
    ///
    /// "There is no screen" has to be a state the sink knows, because the sink outlives the
    /// screen. `q` ends the drawing and lets the run carry on — by design — but the host's
    /// `cfg.approval_sink` still holds this same `Arc<Tui>`, and after `q` there is no
    /// render loop, therefore no key source, therefore nothing in the process that could
    /// complete an approval. A `request` parking there would wait out the run's whole
    /// `max_duration_ms` with nothing drawn: the hang `--headless` and `render/approve.rs`
    /// each exist to prevent, arriving through a third door.
    ///
    /// So a dismissed screen stops being a sink. [`Tui::request`] returns `Err`, and
    /// `Approvals::decide` already maps a sink `Err` to `Denied` — the direction that
    /// closes, and the same answer `--headless` gives.
    dismissed: CancellationToken,
}

impl fmt::Debug for Tui {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tui").finish_non_exhaustive()
    }
}

impl Tui {
    /// A screen and the channel its key presses come out of.
    #[must_use]
    pub fn new() -> (Self, mpsc::Receiver<Intent>) {
        let (intents, rx) = mpsc::channel(8);
        (
            Self {
                state: Mutex::new(AppState::default()),
                intents,
                answer: Mutex::new(None),
                dismissed: CancellationToken::new(),
            },
            rx,
        )
    }

    /// The screen is gone: stop drawing, and stop being an approval sink.
    ///
    /// One-way and idempotent. The host calls it on `q`, and on every other path that takes
    /// the screen down; the render loop calls it on its own way out, so one that ends
    /// because the terminal stopped working closes the sink too. That is what makes "no
    /// screen, no sink" a property of the loop rather than of everyone who starts one.
    pub fn dismiss(&self) {
        self.dismissed.cancel();
    }

    /// Whether the screen has been dismissed.
    #[must_use]
    pub fn is_dismissed(&self) -> bool {
        self.dismissed.is_cancelled()
    }

    /// A snapshot of the screen state, for tests and for the host's final summary.
    #[must_use]
    pub fn snapshot(&self) -> AppState {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Draw one frame from the state as it stands.
    ///
    /// Under the lock rather than over a clone. `draw` takes `&AppState`, so cloning bought
    /// nothing and cost a deep copy of the text buffer, the tool list and its index twenty
    /// times a second — on the worker the pump wants. The lock is held for exactly one
    /// frame, which is shorter than the clone plus the frame it replaced, and there is no
    /// `await` inside it.
    fn draw_frame<B: ratatui::backend::Backend>(
        &self,
        terminal: &mut ratatui::Terminal<B>,
    ) -> std::io::Result<()> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        terminal.draw(|frame| draw(frame, &state))?;
        Ok(())
    }

    /// Redraw on a tick, and turn key presses into [`Intent`]s, until the screen is
    /// dismissed.
    ///
    /// The screen's own token rather than one handed in: "the loop is running" and "the
    /// sink can be answered" are the same fact, and two tokens could disagree about it.
    /// [`Tui::dismiss`] is how a host ends this.
    ///
    /// # Errors
    /// Anything the terminal backend reports while drawing or polling.
    pub async fn render_loop<B: ratatui::backend::Backend>(
        &self,
        terminal: &mut ratatui::Terminal<B>,
    ) -> std::io::Result<()> {
        // `poll` blocks a thread, so it runs on the blocking pool with a short budget: the
        // tick is what bounds how long a cancelled run keeps a terminal in raw mode.
        self.drive(terminal, || read_key(TICK)).await
    }

    /// [`Tui::render_loop`] with the key source handed in.
    ///
    /// Generic over the backend *and* the source because neither is available to a test:
    /// `DefaultTerminal` needs a real terminal, and `crossterm::event::poll` needs a real
    /// one too — it fails outright with "failed to initialize input reader" when stdin is
    /// not a tty, which is every `cargo test`. What is worth pinning here is the shape of
    /// the loop, and the shape does not care where a key comes from.
    async fn drive<B, K, Fut>(
        &self,
        terminal: &mut ratatui::Terminal<B>,
        mut next_key: K,
    ) -> std::io::Result<()>
    where
        B: ratatui::backend::Backend,
        K: FnMut() -> Fut,
        Fut: Future<Output = std::io::Result<Option<KeyEvent>>>,
    {
        // However this loop ends -- dismissed, or a `?` out of a terminal that stopped
        // working -- the screen is gone when it does, and a screen that is gone is not a
        // sink. Written once here rather than at each way out, because the way out that was
        // missed is the one that caused this.
        let _gone = Dismissal(self);
        loop {
            self.draw_frame(terminal)?;
            if self.dismissed.is_cancelled() {
                return Ok(());
            }
            let key = tokio::select! {
                () = self.dismissed.cancelled() => {
                    // One more frame before the screen goes. The host cancels *after* it
                    // has drained the bus, so the events that describe the ending --
                    // `runtime.shutting_down` among them -- folded in while this loop was
                    // parked here. Returning straight away would close the alternate screen
                    // on a frame that predates all of them, which is why the status bar's
                    // "shutting down" segment could be rendered, asserted on, and never once
                    // seen by a user.
                    self.draw_frame(terminal)?;
                    return Ok(());
                }
                key = next_key() => key?,
            };
            if let Some(key) = key {
                self.handle_key(key);
            }
        }
    }

    /// Answer the approval on screen, if a key asked for one.
    ///
    /// Returns whether the key was consumed. A failed `send` means the dispatcher stopped
    /// waiting — the run was cancelled, or its deadline passed — and clearing `pending` on
    /// that failure is what keeps an unanswerable modal off the screen.
    ///
    /// Both locks are held together, `state` then `answer`, which is the order
    /// [`Tui::request`] takes them in too. Setting the two in sequence left a window: a key
    /// arriving between them saw a modal with no sender, took the "nobody is listening"
    /// branch, and cleared `pending` — leaving `request` parked on a receiver that no later
    /// key could reach, because there was no longer a modal on screen to answer.
    fn answer_approval(&self, key: KeyEvent) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut answer = self.answer.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(pending) = state.pending.as_ref() else {
            return false;
        };
        let outcome = match key.code {
            KeyCode::Char('y') => ApprovalOutcome::Approved,
            KeyCode::Char('a') if pending.allow_remember => ApprovalOutcome::ApprovedForSession,
            KeyCode::Char('n') | KeyCode::Esc => ApprovalOutcome::Denied,
            _ => return false,
        };

        let delivered = answer
            .take()
            .is_some_and(|sender| sender.send(outcome).is_ok());
        if !delivered {
            state.pending = None;
        }
        true
    }

    /// Map one key press.
    fn handle_key(&self, key: KeyEvent) {
        // Only presses. A terminal in raw mode also reports releases and repeats, and
        // acting on all three sends `Cancel` up to three times per Ctrl-C.
        if key.kind != KeyEventKind::Press {
            return;
        }
        // Ctrl-C is never taken by the modal: cancelling the run is what clears it, through
        // the dispatcher giving up on the answer. An approval prompt that swallowed the one
        // key a stuck user reaches for would be worse than no prompt.
        if !matches!(key.code, KeyCode::Char('c')) && self.answer_approval(key) {
            return;
        }
        let intent = match (key.code, key.modifiers) {
            // Raw mode swallows SIGINT: Ctrl-C arrives as a key, not a signal. Mapping it
            // to `Cancel` is what keeps Phase 1's guarantee true inside the TUI, and the
            // *second* one is the host's to count -- see `run.rs`. The modifier is ignored
            // on purpose: a user who typed a bare `c` meant to cancel too.
            (KeyCode::Char('c'), _) => Some(Intent::Cancel),
            (KeyCode::Char('q') | KeyCode::Esc, _) => Some(Intent::Quit),
            (KeyCode::Tab, _) => {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.focus = match state.focus {
                    Panel::Agent => Panel::Jobs,
                    Panel::Jobs => Panel::Agent,
                };
                None
            }
            _ => None,
        };
        if let Some(intent) = intent {
            // `try_send` rather than `send`: a full channel means the host is not reading,
            // and blocking here would stop the redraw the user is waiting on. Dropping the
            // duplicate is right -- the channel already holds an unread intent of its own.
            let _ = self.intents.try_send(intent);
        }
    }
}

/// Dismisses the screen however [`Tui::drive`] ends — the `?` and an unwind included.
struct Dismissal<'a>(&'a Tui);

impl Drop for Dismissal<'_> {
    fn drop(&mut self) {
        self.0.dismiss();
    }
}

/// Read one key, waiting at most `timeout`.
///
/// On the blocking pool: `crossterm::event::poll` parks a thread, and parking a runtime
/// worker would stop every other task on it — including the pump feeding this screen.
async fn read_key(timeout: Duration) -> std::io::Result<Option<KeyEvent>> {
    tokio::task::spawn_blocking(move || {
        if !crossterm::event::poll(timeout)? {
            return Ok(None);
        }
        match crossterm::event::read()? {
            TermEvent::Key(key) => Ok(Some(key)),
            _ => Ok(None),
        }
    })
    .await
    .unwrap_or_else(|error| Err(std::io::Error::other(error)))
}

#[async_trait]
impl ApprovalSink for Tui {
    /// Put the prompt on screen and wait for a key — unless the screen is gone.
    ///
    /// No timeout of its own: the run's deadline already reaches the dispatcher, which drops
    /// the receiver when it gives up. The next key press then fails to send, and that
    /// failure is what clears the modal — so the screen never keeps a prompt nobody is
    /// listening for.
    ///
    /// The dismissal is checked *and* raced. Checked, so a question is never put on a screen
    /// that has already gone; raced, so a prompt that was on screen when `q` arrived is
    /// released too rather than parked behind a key that can no longer be pressed.
    async fn request(&self, request: ApprovalRequest) -> rivet_core::Result<ApprovalOutcome> {
        if self.dismissed.is_cancelled() {
            return Err(no_longer_on_screen());
        }
        let (sender, receiver) = oneshot::channel();
        {
            // Both locks, in [`Tui::answer_approval`]'s order, so "a modal on screen has a
            // sender" holds by construction rather than by the window being small.
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let mut answer = self.answer.lock().unwrap_or_else(PoisonError::into_inner);
            state.pending = Some(ApprovalView {
                reason: request.reason,
                preview: request.preview,
                allow_remember: request.allow_remember,
            });
            *answer = Some(sender);
        }

        let outcome = tokio::select! {
            answered = receiver => answered.map_err(|_| {
                rivet_core::Error::cancelled(
                    "the approval prompt was closed before it was answered",
                )
            }),
            () = self.dismissed.cancelled() => Err(no_longer_on_screen()),
        };
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let mut answer = self.answer.lock().unwrap_or_else(PoisonError::into_inner);
            state.pending = None;
            *answer = None;
        }
        outcome
    }
}

/// The refusal a screen that is gone gives.
///
/// `Approvals::decide` maps a sink `Err` to `Denied` with a `tracing::warn!`, so this is the
/// closed direction: the call is refused and the run keeps moving, rather than the run
/// stopping on a question nobody can be shown.
fn no_longer_on_screen() -> rivet_core::Error {
    rivet_core::Error::cancelled(
        "the approval prompt is no longer on screen; the run continued without it",
    )
}

#[async_trait]
impl EventSubscriber for Tui {
    fn name(&self) -> &'static str {
        crate::SUBSCRIBER_NAME
    }

    async fn on_event(&self, envelope: &EventEnvelope) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .apply(envelope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::event::{AgentEvent, Event};

    #[tokio::test]
    async fn quitting_asks_the_host_rather_than_reaching_for_the_token() {
        // The TUI holds no token and no registry. It says what the user wants and stops
        // there; what "quit" costs is the host's decision.
        let (tui, mut intents) = Tui::new();
        tui.handle_key(KeyEvent::new(
            KeyCode::Char('q'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(intents.try_recv().unwrap(), Intent::Quit);
        assert_eq!(
            tui.snapshot().run.turn,
            0,
            "a key press is not an event; it must not touch the fold"
        );
    }

    #[tokio::test]
    async fn ctrl_c_in_the_tui_asks_for_a_cancel() {
        // Raw mode swallows SIGINT, so without this Phase 1's "Ctrl-C stops the run" is
        // quietly false inside `--tui`.
        let (tui, mut intents) = Tui::new();
        tui.handle_key(KeyEvent::new(
            KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert_eq!(intents.try_recv().unwrap(), Intent::Cancel);
    }

    #[tokio::test]
    async fn a_key_release_is_not_a_second_press() {
        let (tui, mut intents) = Tui::new();
        let mut release =
            KeyEvent::new(KeyCode::Char('c'), crossterm::event::KeyModifiers::CONTROL);
        release.kind = KeyEventKind::Release;
        tui.handle_key(release);
        assert!(intents.try_recv().is_err());
    }

    #[tokio::test]
    async fn tab_moves_focus_without_telling_the_host_anything() {
        let (tui, mut intents) = Tui::new();
        assert_eq!(tui.snapshot().focus, Panel::Agent);
        tui.handle_key(KeyEvent::new(
            KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(tui.snapshot().focus, Panel::Jobs);
        assert!(
            intents.try_recv().is_err(),
            "focus is the UI's own business"
        );
    }

    #[tokio::test]
    async fn the_last_frame_shows_the_state_the_run_ended_in() {
        // The bug this pins: the loop drew, then parked in `read_key`, and a cancel while
        // parked returned without drawing again. Everything the host publishes during
        // shutdown -- after it stops the run and before it takes the screen down -- landed
        // in `AppState` and was never rendered, so `runtime.shutting_down` could not reach a
        // user's eyes however correct the fold and the status bar were.
        let (tui, _intents) = Tui::new();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();

        // Concurrent on purpose: the frame at issue is the one drawn *after* the loop has
        // parked, so the event and the dismissal have to arrive while it is parked.
        let feed = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tui.on_event(&EventEnvelope::new(Event::Runtime(
                rivet_core::event::RuntimeEvent::ShuttingDown {
                    reason: "run finished".into(),
                },
            )))
            .await;
            tui.dismiss();
        };
        // A key source that never produces one, so the loop parks exactly where the real
        // `read_key` parks it.
        let idle = || async {
            tokio::time::sleep(TICK).await;
            Ok(None)
        };
        let (drawn, ()) = tokio::join!(tui.drive(&mut terminal, idle), feed);
        drawn.expect("the test backend does not fail");

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            rendered.contains("shutting down"),
            "the frame the user is left looking at predates the end of the run: {rendered}"
        );
    }

    // --- the approval round trip ---------------------------------------------------------

    fn ask(allow_remember: bool) -> rivet_core::policy::ApprovalRequest {
        rivet_core::policy::ApprovalRequest {
            id: rivet_core::id::ApprovalId::new(),
            session_id: rivet_core::id::SessionId::new(),
            reason: "the command matches the destructive shape `rm -rf`".into(),
            preview: "rm -rf build".into(),
            allow_remember,
            scope_key: "shell:rm".into(),
        }
    }

    fn press(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), crossterm::event::KeyModifiers::NONE)
    }

    async fn wait_for_pending(tui: &Tui) {
        for _ in 0..500 {
            if tui.snapshot().pending.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("the prompt never reached the screen");
    }

    #[tokio::test]
    async fn the_keys_map_to_the_three_outcomes() {
        for (key, expected) in [
            ('y', ApprovalOutcome::Approved),
            ('a', ApprovalOutcome::ApprovedForSession),
            ('n', ApprovalOutcome::Denied),
        ] {
            let (tui, _intents) = Tui::new();
            let tui = std::sync::Arc::new(tui);
            let asking = {
                let tui = tui.clone();
                tokio::spawn(async move { tui.request(ask(true)).await })
            };
            wait_for_pending(&tui).await;
            tui.handle_key(press(key));

            assert_eq!(
                asking.await.expect("the task joins").expect("answered"),
                expected,
                "key `{key}`"
            );
            assert!(
                tui.snapshot().pending.is_none(),
                "an answered prompt leaves the screen"
            );
        }
    }

    #[tokio::test]
    async fn the_remember_key_is_inert_when_the_policy_did_not_offer_it() {
        // The shell gate is never rememberable. A prompt that honored `a` anyway would hand
        // out a session-long grant the policy refused to give.
        let (tui, _intents) = Tui::new();
        let tui = std::sync::Arc::new(tui);
        let asking = {
            let tui = tui.clone();
            tokio::spawn(async move { tui.request(ask(false)).await })
        };
        wait_for_pending(&tui).await;

        tui.handle_key(press('a'));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !asking.is_finished(),
            "`a` answered a prompt that never offered it"
        );

        tui.handle_key(press('n'));
        assert_eq!(
            asking.await.expect("the task joins").expect("answered"),
            ApprovalOutcome::Denied
        );
    }

    #[tokio::test]
    async fn a_dropped_receiver_clears_the_pending_modal() {
        // The dispatcher gave up first -- the run was cancelled, or its deadline passed --
        // so the answer has nowhere to go. A screen that kept the prompt would be asking a
        // question nobody is listening for.
        let (tui, _intents) = Tui::new();
        let tui = std::sync::Arc::new(tui);
        let asking = {
            let tui = tui.clone();
            tokio::spawn(async move { tui.request(ask(true)).await })
        };
        wait_for_pending(&tui).await;

        asking.abort();
        let _ = asking.await;

        tui.handle_key(press('y'));
        assert!(
            tui.snapshot().pending.is_none(),
            "the failed send is the signal that clears it"
        );
    }

    #[tokio::test]
    async fn cancelling_is_never_swallowed_by_the_modal() {
        // Ctrl-C is the key a stuck user reaches for. A prompt that ate it would make the
        // approval modal the one place in the UI where a run cannot be stopped.
        let (tui, mut intents) = Tui::new();
        let tui = std::sync::Arc::new(tui);
        let asking = {
            let tui = tui.clone();
            tokio::spawn(async move { tui.request(ask(true)).await })
        };
        wait_for_pending(&tui).await;

        tui.handle_key(press('c'));
        assert_eq!(intents.try_recv().expect("an intent"), Intent::Cancel);
        assert!(
            tui.snapshot().pending.is_some(),
            "the run's own cancellation path is what clears it, not this key"
        );
        asking.abort();
    }

    // --- the screen the user walked away from ---------------------------------------------

    #[tokio::test]
    async fn a_dismissed_screen_is_not_a_sink() {
        // `q` stops the drawing and lets the run finish, which is what it promises. After it
        // there is no render loop, so no key path, so nothing that could ever complete this
        // oneshot -- and the host's `cfg.approval_sink` still holds this same `Tui`. Parking
        // here meant a run that printed nothing and looked hung for its whole
        // `max_duration_ms`: half an hour by default.
        let (tui, _intents) = Tui::new();
        tui.dismiss();

        // Under a timeout, because the regression this pins is a *park*: without the
        // bound, a reintroduced bug would hang the suite instead of failing it.
        let error = tokio::time::timeout(Duration::from_secs(5), tui.request(ask(true)))
            .await
            .expect("the request returns rather than parking")
            .expect_err("a screen that is gone cannot be asked");
        assert_eq!(error.kind(), rivet_core::error::ErrorKind::Cancelled);
        assert!(
            tui.snapshot().pending.is_none(),
            "and nothing was put on a screen nobody is drawing"
        );
    }

    #[tokio::test]
    async fn dismissing_releases_an_approval_that_was_already_waiting() {
        // The same failure, one moment earlier: the prompt was on screen when `q` arrived.
        // Checking the dismissal only on the way in would leave this one parked.
        let (tui, _intents) = Tui::new();
        let tui = std::sync::Arc::new(tui);
        let asking = {
            let tui = tui.clone();
            tokio::spawn(async move { tui.request(ask(true)).await })
        };
        wait_for_pending(&tui).await;

        tui.dismiss();
        let error = tokio::time::timeout(Duration::from_secs(5), asking)
            .await
            .expect("the request returns rather than parking")
            .expect("the task joins")
            .expect_err("a screen that is gone cannot answer");
        assert_eq!(error.kind(), rivet_core::error::ErrorKind::Cancelled);
        assert!(tui.snapshot().pending.is_none());
    }

    #[tokio::test]
    async fn a_render_loop_that_ends_on_its_own_stops_being_a_sink() {
        // Nobody asked this loop to stop -- the terminal did. If only the host's `q` path
        // dismissed, "no screen, no sink" would hold on the paths somebody remembered and
        // not on this one.
        let (tui, _intents) = Tui::new();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let broken = || async { Err(std::io::Error::other("the terminal went away")) };

        tui.drive(&mut terminal, broken)
            .await
            .expect_err("the key source failed");
        assert!(tui.is_dismissed(), "the loop is gone, so the sink is too");
        tokio::time::timeout(Duration::from_secs(5), tui.request(ask(true)))
            .await
            .expect("the request returns rather than parking")
            .expect_err("and it refuses rather than being asked");
    }

    #[tokio::test]
    async fn events_fold_into_the_screen() {
        let (tui, _intents) = Tui::new();
        tui.on_event(&EventEnvelope::new(Event::Agent(AgentEvent::TurnStarted {
            turn: 3,
        })))
        .await;
        assert_eq!(tui.snapshot().status.turn, 3);
    }
}
