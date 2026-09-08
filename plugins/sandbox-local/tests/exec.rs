//! What the local provider actually enforces: the environment, the cap, and the deadline.
//!
//! Every one of these runs a real `sh`, so they are `#[cfg(unix)]` and they use nothing but
//! `sh`, `env`, `sleep` and `yes` — the tests have to pass on CI (ubuntu) and on a
//! developer's machine (darwin), and a GNU extension would pass on one of them.

#![cfg(unix)]

mod support;

use std::time::{Duration, Instant};

use rivet_core::sandbox::ExecSpec;
use support::Fixture;
use tokio_util::sync::CancellationToken;

/// `/bin/sh` by absolute path, because with an empty environment there is no `PATH` to
/// look one up in — which is the property half these tests are about.
fn sh(script: &str) -> ExecSpec {
    ExecSpec::new("/bin/sh", ["-c".to_string(), script.to_string()])
}

#[tokio::test]
async fn the_environment_starts_empty() {
    // `env_clear` first, always. A variable that survives without being named is a
    // credential that leaks without anybody typing it.
    //
    // Note what this cannot assert: `sh` supplies a *default* `PATH` of its own when it
    // starts without one, so the child always has some `PATH`. What matters is that it is
    // not the host's, and that a variable the shell does not invent does not survive.
    let host_path = std::env::var("PATH").expect("the host has a PATH");
    assert!(
        std::env::var("HOME").is_ok_and(|value| !value.is_empty()),
        "this test rests on the host having a HOME"
    );
    let fixture = Fixture::with_passthrough(&[]).await;

    let output = fixture
        .handle
        .exec(
            sh("echo \"[$HOME]\"; echo \"[$PATH]\""),
            CancellationToken::new(),
        )
        .await
        .expect("sh runs");
    let mut lines = output.stdout.lines();
    assert_eq!(lines.next(), Some("[]"), "{output:?}");
    assert_ne!(
        lines.next(),
        Some(format!("[{host_path}]").as_str()),
        "the child inherited the host's PATH: {output:?}"
    );
}

#[tokio::test]
async fn only_the_passthrough_names_survive() {
    assert!(
        std::env::var("PATH").is_ok_and(|value| !value.is_empty()),
        "this test rests on the host having a PATH"
    );
    let fixture = Fixture::with_passthrough(&["PATH"]).await;

    let output = fixture
        .handle
        .exec(
            sh("if [ -n \"$PATH\" ]; then echo kept; fi; echo \"[$HOME]\""),
            CancellationToken::new(),
        )
        .await
        .expect("sh runs");
    assert_eq!(
        output.stdout.trim(),
        "kept\n[]",
        "the listed name came through and the unlisted one did not: {output:?}"
    );
}

#[tokio::test]
async fn a_spec_variable_wins_over_the_passthrough() {
    // A caller that set a name explicitly meant that value, whatever the host holds.
    let fixture = Fixture::with_passthrough(&["PATH"]).await;

    let mut spec = sh("echo \"[$PATH]\"");
    spec.env
        .insert("PATH".to_string(), "/rivet-test-bin".to_string());
    let output = fixture
        .handle
        .exec(spec, CancellationToken::new())
        .await
        .expect("sh runs");
    assert_eq!(output.stdout.trim(), "[/rivet-test-bin]", "{output:?}");
}

#[tokio::test]
async fn the_child_starts_in_the_workspace() {
    let fixture = Fixture::new().await;
    let output = fixture
        .handle
        .exec(sh("pwd"), CancellationToken::new())
        .await
        .expect("sh runs");
    let real = std::fs::canonicalize(fixture.path()).unwrap();
    assert_eq!(output.stdout.trim(), real.to_string_lossy());
}

#[tokio::test]
async fn output_over_the_cap_is_truncated_and_the_child_is_not_blocked() {
    // Ten times the cap. If the reader stopped at the cap the pipe would fill, the child
    // would block in `write`, and this would end at the timeout rather than at the exit --
    // "too much output" wearing the costume of "hung".
    let fixture = Fixture::new().await;
    let mut spec = sh("i=0; while [ $i -lt 100 ]; do printf '%01000d' $i; i=$((i+1)); done");
    spec.max_output_bytes = Some(10_000);
    spec.timeout_ms = Some(10_000);

    let output = fixture
        .handle
        .exec(spec, CancellationToken::new())
        .await
        .expect("sh runs");
    assert_eq!(output.exit_code, Some(0), "the child ran to completion");
    assert!(!output.timed_out, "it finished, it did not stall");
    assert!(output.truncated, "and it says the rest was thrown away");
    assert!(
        output.stdout.len() <= 10_000,
        "kept {} bytes",
        output.stdout.len()
    );
}

#[tokio::test]
async fn a_timeout_kills_the_group_and_reports_timed_out() {
    let fixture = Fixture::new().await;
    let mut spec = sh("sleep 30");
    spec.timeout_ms = Some(300);

    let started = Instant::now();
    let output = fixture
        .handle
        .exec(spec, CancellationToken::new())
        .await
        .expect("the wait ends");
    assert!(output.timed_out, "{output:?}");
    assert_eq!(output.exit_code, None, "a signalled process has no code");
    // Generous: the assertion is that it did not sit out the sleep, not that the kill was
    // fast. The margin is well over the 250 ms term-to-kill grace plus the deadline.
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_cancelled_exec_reports_neither_a_code_nor_a_timeout() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        stopper.cancel();
    });

    let output = fixture
        .handle
        .exec(sh("sleep 30"), cancel)
        .await
        .expect("the wait ends");
    assert_eq!(output.exit_code, None);
    assert!(
        !output.timed_out,
        "a cancellation is not a deadline; the log has to be able to tell them apart"
    );
}

#[tokio::test]
async fn a_grandchild_holding_the_pipe_does_not_wedge_a_finished_call() {
    // The failure `settle()` exists for, and the one defect found by writing tests that had
    // no test of its own.
    //
    // A pipe closes when the *last* writer lets go, and a grandchild inherits its parent's
    // stdout. So this child exits immediately while a process that outlives it keeps the
    // write end open. Awaiting the reader to EOF -- the obvious implementation -- hangs the
    // dispatcher on a call that has already finished.
    //
    // A regression would be a hang rather than a wrong value, so the deadline is the
    // assertion: `timeout` turns "it never came back" into a failure a reader can act on
    // instead of a test run that has to be killed.
    let fixture = Fixture::new().await;
    let started = Instant::now();

    let output = tokio::time::timeout(
        Duration::from_secs(20),
        fixture
            .handle
            .exec(sh("sleep 5 & exit 0"), CancellationToken::new()),
    )
    .await
    .expect("the call never returned: a reader is waiting on a pipe the grandchild holds")
    .expect("the child ran");

    assert_eq!(output.exit_code, Some(0), "the child itself exited cleanly");
    assert!(
        output.truncated,
        "the readers were given up on, and the contract's own flag is how that is said"
    );
    // Bounded well above `OUTPUT_SETTLE` (500 ms) and well below the grandchild's own life
    // (5 s), so this distinguishes "gave up on the readers" from "waited for the pipe".
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}; the grandchild holds the pipe for 5 s, so this waited for it",
        started.elapsed()
    );
}

#[tokio::test]
async fn output_written_before_the_child_exits_still_arrives() {
    // The other half: giving up on the readers must not cost the output of an ordinary
    // command. Nothing outlives this child, so the pipes close and the readers finish.
    let fixture = Fixture::new().await;
    let output = fixture
        .handle
        .exec(sh("echo the marker string"), CancellationToken::new())
        .await
        .expect("sh runs");
    assert_eq!(output.stdout.trim(), "the marker string");
    assert!(!output.truncated, "nothing was given up on: {output:?}");
}

#[tokio::test]
async fn teardown_is_idempotent() {
    let fixture = Fixture::new().await;
    fixture
        .handle
        .exec(sh("true"), CancellationToken::new())
        .await
        .expect("sh runs");
    fixture.handle.teardown().await.expect("first teardown");
    fixture.handle.teardown().await.expect("second teardown");
}

#[tokio::test]
async fn a_non_utf8_stdout_comes_back_lossy_rather_than_failing() {
    // `ExecOutput.stdout` is a `String`, so the bytes a model could not read anyway become
    // replacement characters. `tool-shell` reads that back out and reports it.
    let fixture = Fixture::new().await;
    let output = fixture
        .handle
        .exec(sh("printf '\\377\\376'"), CancellationToken::new())
        .await
        .expect("sh runs");
    assert!(output.stdout.contains('\u{fffd}'), "{output:?}");
}
