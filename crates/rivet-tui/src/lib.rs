//! # rivet-tui
//!
//! The terminal UI. **A pure event-stream consumer**: it depends on `rivet-core` and
//! nothing else of Rivet's, so `use rivet_runtime::…` does not compile here. That is the
//! first of Phase 3's acceptance lines, and it is a property of `Cargo.toml` rather than a
//! lint — `the_tui_crate_does_not_depend_on_the_runtime` parses the manifest to keep it so.
//!
//! ```text
//! bus ──▶ Tui (EventSubscriber) ──▶ AppState::apply   (pure fold)
//!                                        │
//!                        render_loop ────┴──▶ draw(frame, &state)
//!                             │
//!                             └──▶ Intent ──▶ the host's cancellation token
//! ```
//!
//! # The rule this crate is built on
//!
//! **It shows what the events carry, and nothing else.** When the status bar wants
//! something no event carries — the active profile, the workspace root — the answer is to
//! add an event, not an import. Reaching for the runtime to fill one field is how a UI
//! stops being a client of the stream, and the first field is always the cheap one.
//!
//! **Phase 4 adds the one exception, and it is worth naming.** `AppState::pending` is
//! filled by [`app::Tui`]'s [`rivet_core::policy::ApprovalSink`] implementation rather than
//! by a fold over the bus. An approval is not an observation, it is a **round trip**: the
//! bus is one-way and lossy, so a screen that learned about an approval from
//! `tool.approval.requested` would still have nowhere to put the answer. `ApprovalSink` is
//! that somewhere, and it is a `rivet-core` contract — so the rule that actually protects
//! this crate's independence, "no import outside `rivet-core`", is untouched.
//!
//! The corollary is that the job panel is built now, in a phase with no job runtime. Its
//! producer arrives in Phase 5; leaving the panel out until then would leave Phase 5
//! holding an empty panel and a tempting `rivet-job` dependency.

pub mod app;
pub mod draw;
pub mod state;
pub mod terminal;

/// The name this crate's subscriber registers under, and the one a lag report will show.
///
/// At the crate root because two modules need to agree on it: [`app::Tui`] returns it from
/// `EventSubscriber::name`, and [`state::AppState`] compares a `runtime.subscriber_lagged`
/// report against it to tell what *this* screen missed from what somebody else did.
pub const SUBSCRIBER_NAME: &str = "render.tui";

pub use app::{Intent, Tui};
pub use draw::{NO_JOBS, draw, status_line};
pub use state::{
    AppState, ApprovalView, JobLine, JobView, Panel, RunView, StatusView, ToolLine, ToolStatus,
};
pub use terminal::{TerminalGuard, is_a_terminal};
