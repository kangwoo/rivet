//! Killing a process **group**, safely.
//!
//! `sh -c "cargo test"` makes `cargo` the child and `rustc` a grandchild. `Child::kill`
//! reaches the first and leaves the second, so a cancelled run would keep compiling. The
//! answer is to put the child in a process group of its own at spawn
//! (`Command::process_group(0)`, a safe method) and signal the group.
//!
//! The signal itself goes through `rustix`, not through an `unsafe` call to `libc::killpg`:
//! the workspace sets `unsafe_code = "forbid"`, and this is the same shape
//! `rivet_runtime::fsguard` uses for `O_NOFOLLOW` — a platform primitive reached through a
//! safe wrapper.
//!
//! # Windows
//!
//! There is no process-group kill here, so this provider kills only the child on that
//! platform and a grandchild can survive. `fsguard` documents the same asymmetry for
//! symlinks, and for the same reason: the tests are `#[cfg(unix)]` and Windows is
//! **not covered**.

/// How long a group has to exit after `SIGTERM` before `SIGKILL`.
///
/// Short, because the caller is already out of patience: this runs after a timeout or a
/// cancellation, both of which have their own budget upstream. Long enough for a shell to
/// pass the signal on to its children.
pub const TERM_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// Ask a process group to stop, then make it.
///
/// Returns whether a signal was delivered at all — `false` on a platform without process
/// groups, and on a group that has already exited.
#[cfg(unix)]
pub async fn terminate(pgid: u32) -> bool {
    let Ok(raw) = i32::try_from(pgid) else {
        return false;
    };
    let Some(leader) = rustix::process::Pid::from_raw(raw) else {
        return false;
    };
    let asked = rustix::process::kill_process_group(leader, rustix::process::Signal::TERM).is_ok();
    if !asked {
        return false;
    }
    tokio::time::sleep(TERM_GRACE).await;
    // Unconditional: a group that already exited answers `ESRCH`, which is the result we
    // wanted anyway. Checking first would be a race, not a saving.
    let _ = rustix::process::kill_process_group(leader, rustix::process::Signal::KILL);
    true
}

/// No process groups here; the caller falls back to killing the child alone.
#[cfg(not(unix))]
pub async fn terminate(_pgid: u32) -> bool {
    false
}
