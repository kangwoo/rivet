//! What each tool asks the host to run, and what it does with the answer.

mod support;

use rivet_core::sandbox::ExecOutput;
use rivet_core::tool::Tool;
use rivet_tool_git::{GitCommit, GitDiff, GitLog, GitStatus};
use support::Fixture;

#[tokio::test]
async fn status_runs_git_through_the_host_and_never_directly() {
    let fixture = Fixture::new();
    GitStatus
        .execute(fixture.ctx(), serde_json::json!({}))
        .await
        .expect("the spy answers");

    let spec = fixture.host.only_call();
    assert_eq!(spec.program, "git");
    assert_eq!(spec.args, ["--no-pager", "status", "--short", "--branch"]);
}

#[tokio::test]
async fn every_invocation_disables_the_pager() {
    // A pager waiting for a keypress is a hang, and `-c core.pager=cat` would be config
    // injection -- the reason `.git/config` is denied in the first place.
    let fixture = Fixture::new();
    for input in [
        serde_json::json!({}),
        serde_json::json!({ "staged": true }),
        serde_json::json!({ "limit": 5 }),
    ] {
        let spy = Fixture::new();
        let _ = GitDiff.execute(spy.ctx(), input.clone()).await;
        assert_eq!(spy.host.only_call().args[0], "--no-pager");
    }
    drop(fixture);
}

#[tokio::test]
async fn a_staged_diff_against_a_revision_builds_the_argv_in_order() {
    let fixture = Fixture::new();
    GitDiff
        .execute(
            fixture.ctx(),
            serde_json::json!({ "staged": true, "rev": "HEAD~1", "path": "src" }),
        )
        .await
        .expect("the spy answers");

    assert_eq!(
        fixture.host.only_call().args,
        ["--no-pager", "diff", "--staged", "HEAD~1", "--", "src"],
        "`--` has to come before the path, or a path that looks like a revision is one"
    );
}

#[tokio::test]
async fn log_defaults_to_twenty_and_honors_a_limit() {
    let fixture = Fixture::new();
    GitLog
        .execute(fixture.ctx(), serde_json::json!({}))
        .await
        .expect("the spy answers");
    // `--max-count 20`, not `--max-count=20`: the separate form binds the number to the
    // option, so it is never an argv entry of its own. Nothing about *this* argument was
    // dangerous -- the schema bounds it to an integer -- but `Argv` has one shape for "a
    // value the model influenced", and a tool that used a second shape for the safe cases
    // would be a tool where the safe cases are the ones nobody checks.
    let args = fixture.host.only_call().args;
    let index = args
        .iter()
        .position(|a| a == "--max-count")
        .expect("the limit reached argv");
    assert_eq!(args[index + 1], "20");

    let asked = Fixture::new();
    GitLog
        .execute(asked.ctx(), serde_json::json!({ "limit": 3 }))
        .await
        .expect("the spy answers");
    let args = asked.host.only_call().args;
    let index = args
        .iter()
        .position(|a| a == "--max-count")
        .expect("the limit reached argv");
    assert_eq!(args[index + 1], "3");
}

#[tokio::test]
async fn a_commit_message_is_its_own_argv_entry() {
    // Whatever the model wrote, nothing in the message is ever parsed as a flag.
    let fixture = Fixture::new();
    GitCommit
        .execute(
            fixture.ctx(),
            serde_json::json!({ "message": "--amend --author=someone", "all": true }),
        )
        .await
        .expect("the spy answers");

    assert_eq!(
        fixture.host.only_call().args,
        [
            "--no-pager",
            "commit",
            "--all",
            "-m",
            "--amend --author=someone"
        ]
    );
}

#[tokio::test]
async fn a_non_repository_is_a_result_the_model_reads() {
    let fixture = Fixture::new();
    fixture.host.answers_with(ExecOutput {
        exit_code: Some(128),
        stdout: String::new(),
        stderr: "fatal: not a git repository".into(),
        timed_out: false,
        truncated: false,
        duration_ms: 2,
    });

    let result = GitStatus
        .execute(fixture.ctx(), serde_json::json!({}))
        .await
        .expect("a failing git is not a runtime failure");
    assert!(result.is_error);
    assert!(
        result.content.contains("not a git repository"),
        "{result:?}"
    );
}

#[tokio::test]
async fn the_contexts_budget_and_cap_reach_the_spec() {
    let fixture = Fixture::new();
    GitStatus
        .execute(fixture.ctx(), serde_json::json!({}))
        .await
        .expect("the spy answers");
    let spec = fixture.host.only_call();
    assert_eq!(spec.timeout_ms, Some(30_000));
    assert_eq!(spec.max_output_bytes, Some(65_536));
}
