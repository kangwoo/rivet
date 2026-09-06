//! Ctrl-C handling.
//!
//! The first interrupt asks the run to stop and lets it record what happened — the
//! five-second budget in `rivet_runtime::agent_loop`. The second gives up on that and
//! kills the process, because a runtime you cannot interrupt twice is a runtime you have
//! to `kill -9`, and that is the case the session log then has to recover from.

use tokio_util::sync::CancellationToken;

/// Watch for interrupts until the run finishes.
///
/// Returns a handle the caller should abort once the run is over.
pub fn install(cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_err() {
            return;
        }
        eprintln!("\nstopping… (press Ctrl-C again to force)");
        cancel.cancel();

        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("forced; the session log may end mid-turn and will be repaired on resume");
            std::process::exit(crate::exit::CANCELLED);
        }
    })
}
