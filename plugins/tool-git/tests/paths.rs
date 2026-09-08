//! A `path` argument cannot leave the workspace, including through a link.
//!
//! The other half of this phase's symlink gate. `sandbox-local` checks a process's `cwd`;
//! this checks the path these tools put in argv, because `git -- ../secrets` would otherwise
//! be a perfectly ordinary way to read outside the workspace with `git` doing the reading.
//!
//! `#[cfg(unix)]` for the link cases: creating a symlink on Windows needs a privilege, and
//! `fsguard` records the same gap for the same reason.

mod support;

use rivet_core::tool::Tool;
use rivet_tool_git::{GitDiff, GitLog};
use support::Fixture;

#[cfg(unix)]
#[tokio::test]
async fn a_git_path_argument_cannot_leave_the_workspace_through_a_link() {
    let outside = tempfile::tempdir().expect("a directory outside the workspace");
    std::fs::write(outside.path().join("secrets"), "shh").expect("write");
    let fixture = Fixture::new();
    std::os::unix::fs::symlink(outside.path(), fixture.path().join("escape")).expect("symlink");

    let error = GitDiff
        .execute(fixture.ctx(), serde_json::json!({ "path": "escape" }))
        .await
        .expect_err("a link out of the workspace is not a path in it");
    assert_eq!(error.kind(), rivet_core::error::ErrorKind::PolicyDenied);
    assert_eq!(
        fixture.host.calls(),
        0,
        "and `git` was never started with it"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_link_that_stays_inside_is_resolved_rather_than_refused() {
    // Links inside a repository are ordinary. The *real* path is what gets checked, so
    // nothing about containment is given up by allowing this.
    let fixture = Fixture::new();
    std::os::unix::fs::symlink("src", fixture.path().join("code")).expect("symlink");

    GitLog
        .execute(fixture.ctx(), serde_json::json!({ "path": "code" }))
        .await
        .expect("a link inside the workspace resolves");
    let args = fixture.host.only_call().args;
    assert_eq!(
        args.last().map(String::as_str),
        Some("src"),
        "the real path is what reaches argv: {args:?}"
    );
}

#[tokio::test]
async fn a_lexically_escaping_path_is_refused() {
    let fixture = Fixture::new();
    let error = GitDiff
        .execute(fixture.ctx(), serde_json::json!({ "path": "../elsewhere" }))
        .await
        .expect_err("`..` never leaves the workspace");
    assert_eq!(error.kind(), rivet_core::error::ErrorKind::PolicyDenied);
    assert_eq!(fixture.host.calls(), 0);
}

#[tokio::test]
async fn a_path_reaches_argv_relative_to_the_workspace() {
    let fixture = Fixture::new();
    GitDiff
        .execute(fixture.ctx(), serde_json::json!({ "path": "src/main.rs" }))
        .await
        .expect("a real file inside the workspace");
    let args = fixture.host.only_call().args;
    assert_eq!(
        args.last().map(String::as_str),
        Some("src/main.rs"),
        "an absolute path would be a second way to say the same thing: {args:?}"
    );
}
