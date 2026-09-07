//! Raw mode, and getting out of it.
//!
//! A terminal left in raw mode after a panic is a shell that no longer echoes what you
//! type. So entry and exit are tied to a value's lifetime, and a panic hook covers the case
//! where nothing gets to be dropped at all.
//!
//! `Drop` is the right tool *here* — unlike `Sandbox`, which cannot use it because its
//! teardown has to `await`. Leaving raw mode is two synchronous ioctls.

use std::fmt;
use std::io;
use std::sync::Once;

use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};

/// Installs the panic hook exactly once per process.
static HOOK: Once = Once::new();

/// Owns the terminal's raw mode and alternate screen for as long as it lives.
pub struct TerminalGuard {
    terminal: Option<ratatui::DefaultTerminal>,
}

impl fmt::Debug for TerminalGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalGuard")
            .field("active", &self.terminal.is_some())
            .finish()
    }
}

impl TerminalGuard {
    /// Enter raw mode and the alternate screen.
    ///
    /// # Errors
    /// Whatever the terminal reports. A pipe has no raw mode, and the caller should refuse
    /// `--tui` before getting here — see [`is_a_terminal`].
    pub fn enter() -> io::Result<Self> {
        HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                // Best effort, and unconditional: a panic can happen with no guard alive,
                // and leaving a shell without echo is worse than one redundant reset.
                restore();
                previous(info);
            }));
        });
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(out, EnterAlternateScreen)?;
        let terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(out))?;
        Ok(Self {
            terminal: Some(terminal),
        })
    }

    /// The terminal to draw on.
    ///
    /// # Panics
    /// After [`TerminalGuard::restore`] has been called.
    pub fn terminal(&mut self) -> &mut ratatui::DefaultTerminal {
        self.terminal
            .as_mut()
            .expect("the guard was already restored")
    }

    /// Put the terminal back now, rather than at drop.
    ///
    /// The host calls this before printing its summary, so the summary lands on the real
    /// screen instead of the alternate one that is about to be discarded. Idempotent, and
    /// `Drop` still runs after it — [`restore`] is safe to call twice.
    pub fn restore(&mut self) {
        if self.terminal.take().is_some() {
            restore();
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Leave raw mode and the alternate screen, ignoring errors.
///
/// Free-standing because the panic hook needs it without a guard, and because
/// `std::process::exit` — which the forced-quit path takes — runs no destructors.
pub fn restore() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
}

/// Whether stdout is a terminal.
///
/// The host checks this before entering raw mode: putting a pipe into raw mode leaves no
/// terminal to restore, and the failure shows up later as a shell that stopped echoing.
#[must_use]
pub fn is_a_terminal() -> bool {
    std::io::IsTerminal::is_terminal(&io::stdout())
}
