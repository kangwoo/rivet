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

#[tokio::test]
async fn the_whole_workspace_is_a_path_a_model_can_ask_for() {
    // `path: "."` resolves to the workspace root, which strips to the empty string --
    // and `git --no-pager diff -- ""` exits 128 with "empty string is not a valid
    // pathspec. please use . instead if you meant to match all paths". Git's own advice is
    // the input that produced the error, so a model that follows it loops. An empty
    // pathspec means "everything", and so does emitting none, which is what `Argv` now
    // does with it.
    for spelling in [".", "./"] {
        let fixture = Fixture::new();
        GitDiff
            .execute(fixture.ctx(), serde_json::json!({ "path": spelling }))
            .await
            .unwrap_or_else(|e| panic!("`{spelling}` is the workspace: {e}"));
        let args = fixture.host.only_call().args;
        assert!(
            !args.iter().any(String::is_empty),
            "`{spelling}`: git rejects an empty pathspec outright: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--"),
            "`{spelling}`: a separator with nothing after it is what produced the empty \
             pathspec: {args:?}"
        );
    }
}

#[tokio::test]
async fn the_whole_workspace_is_a_path_git_log_can_ask_for_too() {
    // The same door, the other caller. Guarding `Argv::pathspec` rather than both call
    // sites is why this one needed no separate fix.
    let fixture = Fixture::new();
    GitLog
        .execute(fixture.ctx(), serde_json::json!({ "path": "." }))
        .await
        .expect("`.` is the workspace");
    let args = fixture.host.only_call().args;
    assert!(!args.iter().any(|a| a.is_empty() || a == "--"), "{args:?}");
}

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
