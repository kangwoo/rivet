//! The symlink gate for a process's working directory.
//!
//! Phase 1.6b closed the path a *file* is opened through. A process opens nothing: it is
//! handed a `cwd` and starts there. So the gate for this phase's new surface is here, and it
//! is the same mechanism — `fsguard::resolve_dir` canonicalizes the candidate and re-runs
//! containment on the real path, which is what a lexical check cannot do.
//!
//! `#[cfg(unix)]`, because creating a symlink on Windows needs a privilege. That platform is
//! **not covered**, the same gap `fsguard` documents for itself.

#![cfg(unix)]

mod support;

use rivet_core::sandbox::ExecSpec;
use support::Fixture;
use tokio_util::sync::CancellationToken;

fn sh(script: &str) -> ExecSpec {
    ExecSpec::new("/bin/sh", ["-c".to_string(), script.to_string()])
}

#[tokio::test]
async fn a_symlinked_cwd_pointing_outside_the_workspace_is_refused() {
    let outside = tempfile::tempdir().expect("a directory outside the workspace");
    let fixture = Fixture::new().await;
    std::os::unix::fs::symlink(outside.path(), fixture.path().join("escape")).expect("symlink");

    let mut spec = sh("pwd");
    spec.cwd = Some(std::path::PathBuf::from("escape"));
    let error = fixture
        .handle
        .exec(spec, CancellationToken::new())
        .await
        .expect_err("a link out of the workspace is not a working directory");
    assert_eq!(error.kind(), rivet_core::error::ErrorKind::PolicyDenied);
}

#[tokio::test]
async fn a_symlinked_cwd_pointing_inside_the_workspace_is_allowed() {
    // Refusing this would be safe and unjustified. Links inside a repository are ordinary,
    // and the real path is checked, so nothing about containment is given up.
    let fixture = Fixture::new().await;
    std::fs::create_dir(fixture.path().join("src")).expect("mkdir");
    std::os::unix::fs::symlink("src", fixture.path().join("code")).expect("symlink");

    let mut spec = sh("pwd");
    spec.cwd = Some(std::path::PathBuf::from("code"));
    let output = fixture
        .handle
        .exec(spec, CancellationToken::new())
        .await
        .expect("a link that stays inside is fine");
    assert!(output.stdout.trim().ends_with("src"), "{output:?}");
}

#[tokio::test]
async fn a_lexically_escaping_cwd_is_refused_too() {
    let fixture = Fixture::new().await;
    let mut spec = sh("pwd");
    spec.cwd = Some(std::path::PathBuf::from("../.."));
    let error = fixture
        .handle
        .exec(spec, CancellationToken::new())
        .await
        .expect_err("`..` never leaves the workspace either");
    assert_eq!(error.kind(), rivet_core::error::ErrorKind::PolicyDenied);
}

#[tokio::test]
async fn an_absent_cwd_is_the_workspace_root() {
    let fixture = Fixture::new().await;
    let output = fixture
        .handle
        .exec(sh("pwd"), CancellationToken::new())
        .await
        .expect("sh runs");
    let real = std::fs::canonicalize(fixture.path()).unwrap();
    assert_eq!(output.stdout.trim(), real.to_string_lossy());
}
