//! No model-supplied string reaches `git` in a position it would read as an option.
//!
//! # Why this iterates the schema instead of listing arguments
//!
//! The bug this replaces was not a missing check, it was a check applied per call site.
//! `path` was pushed after a `--` separator precisely so "a path that looks like a revision
//! is still a path"; `rev` was pushed two lines above it with nothing at all, and
//! `git --no-pager diff --output=../x` exits 0 and writes a file above the working
//! directory. A test that named `rev` would have caught `rev` and not the next one.
//!
//! So the test walks `spec().input_schema` and feeds **every declared string property** an
//! option-shaped value. An argument added later is covered on the day it is declared,
//! because declaring it is what a tool has to do to receive it at all.

mod support;

use rivet_core::tool::Tool;
use rivet_runtime::argv::option_exposure;
use rivet_tool_git::{GitCommit, GitDiff, GitLog, GitStatus};
use support::Fixture;

/// Option-shaped, and the specific one that made this a vulnerability rather than a
/// curiosity: `git` writes the diff wherever it points, from a tool annotated `read_only`.
const INJECTED: &str = "--output=escaped-by-the-model";

/// Every string property a tool declares, in schema order.
fn string_properties(tool: &dyn Tool) -> Vec<String> {
    let spec = tool.spec();
    let Some(properties) = spec.input_schema["properties"].as_object() else {
        return Vec::new();
    };
    properties
        .iter()
        .filter(|(_, schema)| schema["type"] == "string")
        .map(|(key, _)| key.clone())
        .collect()
}

/// The input a tool needs before the key under test is added.
fn required_scaffold(
    tool: &dyn Tool,
    under_test: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let spec = tool.spec();
    let mut input = serde_json::Map::new();
    if let Some(required) = spec.input_schema["required"].as_array() {
        for key in required.iter().filter_map(serde_json::Value::as_str) {
            if key != under_test {
                input.insert(key.to_string(), serde_json::json!("benign"));
            }
        }
    }
    input
}

#[tokio::test]
async fn no_declared_argument_can_become_an_option() {
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(GitStatus),
        Box::new(GitDiff),
        Box::new(GitLog),
        Box::new(GitCommit),
    ];

    let mut exercised: Vec<String> = Vec::new();
    for tool in &tools {
        let name = tool.spec().name;
        for key in string_properties(tool.as_ref()) {
            exercised.push(format!("{name}.{key}"));
            let fixture = Fixture::new();
            let mut input = required_scaffold(tool.as_ref(), &key);
            input.insert(key.clone(), serde_json::json!(INJECTED));

            let outcome = tool
                .execute(fixture.ctx(), serde_json::Value::Object(input))
                .await;

            if outcome.is_err() {
                // Refused before anything ran. `rev` lands here, and so does a `path` that
                // does not resolve inside the workspace.
                assert_eq!(
                    fixture.host.calls(),
                    0,
                    "{name}.{key}: refused, but `git` was started anyway"
                );
            } else {
                // Allowed through, so it must be somewhere `git` reads as a value: past the
                // `--` separator, or bound to the option before it.
                let args = fixture.host.only_call().args;
                assert_eq!(
                    option_exposure(&args, INJECTED),
                    None,
                    "{name}.{key}: reached argv where `git` would parse it as an option: \
                     {args:?}"
                );
            }
        }
    }
    // What the walk found, named. The *coverage* above is automatic -- a property added
    // later is exercised the day it is declared -- and this line only proves the walk
    // walked. A new argument fails here too, which is the point: adding one should make
    // somebody look at this file once.
    exercised.sort();
    assert_eq!(
        exercised,
        [
            "git_commit.message",
            "git_diff.path",
            "git_diff.rev",
            "git_log.path"
        ],
        "the schema walk found a different set of string arguments than the tools declare"
    );
}

#[tokio::test]
async fn a_revision_that_looks_like_a_flag_is_refused_by_name() {
    // The reported vulnerability, spelled out rather than derived: `readonly` and `reviewer`
    // both register `git_diff`, and both promise no `fs_write` at all.
    let fixture = Fixture::new();
    let error = GitDiff
        .execute(
            fixture.ctx(),
            serde_json::json!({ "rev": "--output=../../pwned" }),
        )
        .await
        .expect_err("a revision beginning with `-` is an option, not a revision");

    assert_eq!(
        error.kind(),
        rivet_core::error::ErrorKind::PolicyDenied,
        "a containment refusal, so the dispatcher records it as `tool.blocked` rather than \
         letting it scroll past as a tool result"
    );
    assert!(error.message().contains("rev"), "{error}");
    assert_eq!(
        fixture.host.calls(),
        0,
        "and `git` was never started with it"
    );
}

#[tokio::test]
async fn an_ordinary_revision_still_works() {
    // The refusal has to be narrow enough to leave the tool useful. No revision `git`
    // resolves begins with `-`: `HEAD~1`, `main`, `origin/main`, `v1.0`, `abc123`.
    for rev in ["HEAD~1", "main", "origin/main", "v1.0", "abc1234"] {
        let fixture = Fixture::new();
        GitDiff
            .execute(fixture.ctx(), serde_json::json!({ "rev": rev }))
            .await
            .unwrap_or_else(|e| panic!("`{rev}` is an ordinary revision: {e}"));
        assert!(
            fixture.host.only_call().args.contains(&rev.to_string()),
            "`{rev}` did not reach argv"
        );
    }
}

#[tokio::test]
async fn a_commit_message_may_still_begin_with_a_dash() {
    // `-m` consumes the next argv entry verbatim, so this was never dangerous, and refusing
    // it would be refusing something a person may legitimately want to write.
    let fixture = Fixture::new();
    GitCommit
        .execute(
            fixture.ctx(),
            serde_json::json!({ "message": "--wip: do not ship" }),
        )
        .await
        .expect("a message bound to `-m` is a message");

    let args = fixture.host.only_call().args;
    let index = args
        .iter()
        .position(|a| a == "--wip: do not ship")
        .expect("the message reached argv");
    assert_eq!(args[index - 1], "-m", "and it is still bound to `-m`");
    assert_eq!(option_exposure(&args, "--wip: do not ship"), None);
}
