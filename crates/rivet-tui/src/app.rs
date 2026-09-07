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
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::draw::draw;
use crate::state::{AppState, Panel};

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
            },
            rx,
        )
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

    /// Redraw on a tick, and turn key presses into [`Intent`]s, until `cancel` fires.
    ///
    /// # Errors
    /// Anything the terminal backend reports while drawing or polling.
    pub async fn render_loop<B: ratatui::backend::Backend>(
        &self,
        terminal: &mut ratatui::Terminal<B>,
        cancel: CancellationToken,
    ) -> std::io::Result<()> {
        // `poll` blocks a thread, so it runs on the blocking pool with a short budget: the
        // tick is what bounds how long a cancelled run keeps a terminal in raw mode.
        self.drive(terminal, cancel, || read_key(TICK)).await
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
        cancel: CancellationToken,
        mut next_key: K,
    ) -> std::io::Result<()>
    where
        B: ratatui::backend::Backend,
        K: FnMut() -> Fut,
        Fut: Future<Output = std::io::Result<Option<KeyEvent>>>,
    {
        loop {
            self.draw_frame(terminal)?;
            if cancel.is_cancelled() {
                return Ok(());
            }
            let key = tokio::select! {
                () = cancel.cancelled() => {
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

    /// Map one key press.
    fn handle_key(&self, key: KeyEvent) {
        // Only presses. A terminal in raw mode also reports releases and repeats, and
        // acting on all three sends `Cancel` up to three times per Ctrl-C.
        if key.kind != KeyEventKind::Press {
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
        let cancel = CancellationToken::new();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();

        // Concurrent on purpose: the frame at issue is the one drawn *after* the loop has
        // parked, so the event and the cancel have to arrive while it is parked.
        let feed = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tui.on_event(&EventEnvelope::new(Event::Runtime(
                rivet_core::event::RuntimeEvent::ShuttingDown {
                    reason: "run finished".into(),
                },
            )))
            .await;
            cancel.cancel();
        };
        // A key source that never produces one, so the loop parks exactly where the real
        // `read_key` parks it.
        let idle = || async {
            tokio::time::sleep(TICK).await;
            Ok(None)
        };
        let (drawn, ()) = tokio::join!(tui.drive(&mut terminal, cancel.clone(), idle), feed);
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
