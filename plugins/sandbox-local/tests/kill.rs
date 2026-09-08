//! No orphans: cancelling takes the whole process tree, not just the child.
//!
//! `sh -c "cargo test"` makes `cargo` the child and `rustc` a grandchild. Killing the child
//! alone leaves the grandchild compiling after the run is over, which is the correctness bug
//! this phase's acceptance line names. The fix is the process *group*, and the check is that
//! a grandchild is gone afterwards.
//!
//! # How the grandchild is observed
//!
//! The script writes the grandchild's pid to a file (`$!`), and the test asks `kill -0`
//! about it afterwards — POSIX, present on both platforms these tests run on, and needing
//! neither `/proc` (absent on darwin) nor a parse of `ps` output (different on each). A pipe
//! held open would be the other way to see it, but a FIFO opened for reading blocks until
//! somebody opens the write end, so a test built on one deadlocks exactly when it passes.

#![cfg(unix)]

mod support;

use std::time::{Duration, Instant};

use rivet_core::sandbox::ExecSpec;
use support::Fixture;
use tokio_util::sync::CancellationToken;

/// A shell that writes its background child's pid to `pid`, then sleeps.
///
/// The outer `sh` is the sandbox's child; the backgrounded `sleep` is the grandchild, and
/// the grandchild is what a plain `Child::kill` would leave behind.
fn tree_with_a_grandchild() -> ExecSpec {
    ExecSpec::new(
        "/bin/sh",
        [
            "-c".to_string(),
            "sleep 300 & echo $! > pid; sleep 300".to_string(),
        ],
    )
}

/// The pid the script wrote, once it has written one.
async fn grandchild_pid(path: &std::path::Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the script never reported a grandchild pid");
}

/// Whether the process is gone, waited for up to `within`.
async fn gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        let alive = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("kill -0 {pid} 2>/dev/null"))
            .status()
            .await
            .expect("sh runs")
            .success();
        if !alive {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn cancelling_kills_the_whole_process_tree() {
    let fixture = Fixture::new().await;
    let pid_file = fixture.path().join("pid");

    let cancel = CancellationToken::new();
    let running = {
        let cancel = cancel.clone();
        let fixture = &fixture;
        async move { fixture.handle.exec(tree_with_a_grandchild(), cancel).await }
    };
    let stop = {
        let pid_file = pid_file.clone();
        let cancel = cancel.clone();
        async move {
            let pid = grandchild_pid(&pid_file).await;
            cancel.cancel();
            pid
        }
    };

    let (output, pid) = tokio::join!(running, stop);
    let output = output.expect("the wait ends");
    assert_eq!(output.exit_code, None, "a signalled process has no code");
    assert!(
        gone(pid, Duration::from_secs(10)).await,
        "grandchild {pid} outlived the run: killing the child alone leaves \
         `sh -c \"cargo test\"` compiling after the run is over"
    );
}

#[tokio::test]
async fn teardown_kills_a_process_the_caller_walked_away_from() {
    // The dispatcher's half of the same guarantee: a tool task that ignores cancellation is
    // abandoned rather than aborted, so whoever still holds the handle has to be able to
    // kill what it started.
    let fixture = std::sync::Arc::new(Fixture::new().await);
    let pid_file = fixture.path().join("pid");

    let running = {
        let fixture = fixture.clone();
        tokio::spawn(async move {
            fixture
                .handle
                .exec(tree_with_a_grandchild(), CancellationToken::new())
                .await
        })
    };
    let pid = grandchild_pid(&pid_file).await;

    fixture.handle.teardown().await.expect("teardown");
    assert!(
        gone(pid, Duration::from_secs(10)).await,
        "teardown left grandchild {pid} running"
    );
    running.abort();
}
