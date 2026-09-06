//! The `rivet` binary, driven as a subprocess against a loopback provider.
//!
//! These are the tests that prove the pieces are actually wired to each other: config
//! discovery, plugin registration, the session store on disk, the dispatcher, the
//! filesystem tools, and the exit codes a script will branch on.

mod support;

use std::time::{Duration, Instant};

use support::{Provider, Workspace, sse_text, sse_tool_call, tool_results};

#[tokio::test]
async fn a_prompt_runs_a_tool_and_the_model_sees_its_result() {
    // The plan's representative command, end to end: ask, read a file, answer.
    let provider = Provider::start(vec![
        sse_tool_call("read_file", &serde_json::json!({ "path": "src/main.rs" })),
        sse_text("It prints the marker string."),
    ])
    .await;
    let workspace = Workspace::new(&provider.base_url);

    let (code, stdout, stderr) = workspace
        .run(&["list the files here and explain what this project does"])
        .await;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("It prints the marker string."),
        "the answer goes to stdout so `rivet ... > answer.md` works: {stdout}"
    );

    let requests = provider.requests().await;
    assert_eq!(requests.len(), 2);
    let results = tool_results(&requests[1]);
    assert_eq!(results.len(), 1);
    assert!(
        results[0].contains("the marker string"),
        "the model must be shown what the tool produced: {results:?}"
    );

    // And the durable log tells the same story.
    let events = workspace.session_events();
    assert_eq!(
        events,
        [
            "session.created",
            "run.started",
            "user.message",
            "model.requested",
            "assistant.message",
            "tool.called",
            "tool.completed",
            "model.requested",
            "assistant.message",
            "run.completed"
        ]
    );
}

#[tokio::test]
async fn a_denied_path_is_refused_and_the_model_is_told_why() {
    let provider = Provider::start(vec![
        sse_tool_call("read_file", &serde_json::json!({ "path": ".env" })),
        sse_text("I cannot read that file."),
    ])
    .await;
    let workspace = Workspace::new(&provider.base_url);

    let (code, _stdout, stderr) = workspace.run(&["read the env file"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");

    let requests = provider.requests().await;
    let results = tool_results(&requests[1]);
    assert!(
        results[0].contains("Blocked by policy"),
        "the model has to see the refusal or it will just try again: {results:?}"
    );
    assert!(
        !results[0].contains("TOKEN=secret"),
        "and it must not see the secret: {results:?}"
    );
    assert!(
        workspace
            .session_events()
            .contains(&"tool.blocked".to_string()),
        "a refusal is exactly the fact an audit asks about"
    );
}

#[tokio::test]
async fn jsonl_output_is_one_parseable_event_per_line() {
    let provider = Provider::start(vec![sse_text("hello")]).await;
    let workspace = Workspace::new(&provider.base_url);

    let (code, stdout, stderr) = workspace.run(&["--jsonl", "say hello"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty(), "no events: {stdout}");
    for line in &lines {
        let event: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON: {line} ({e})"));
        assert!(event["id"].is_string(), "{event}");
        assert!(event["payload"].is_object(), "{event}");
    }
}

#[tokio::test]
async fn an_unknown_profile_is_a_configuration_error() {
    let provider = Provider::start(vec![sse_text("unused")]).await;
    let workspace = Workspace::new(&provider.base_url);
    let (code, _stdout, stderr) = workspace.run(&["--profile", "readnly", "hello"]).await;
    assert_eq!(code, 2, "configuration problems are exit code 2");
    assert!(stderr.contains("unknown profile"), "{stderr}");
}

#[tokio::test]
async fn a_missing_credential_stops_before_a_session_exists() {
    let provider = Provider::start(vec![sse_text("unused")]).await;
    let workspace = Workspace::new(&provider.base_url);

    let output = workspace
        .command(&["hello"])
        .env_remove(support::KEY_ENV)
        .output()
        .await
        .expect("spawn");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(support::KEY_ENV), "{stderr}");
    assert!(
        workspace.session_log().is_none(),
        "an empty session left behind by a setup mistake is noise forever"
    );
}

#[tokio::test]
async fn doctor_reports_what_the_configuration_resolved_to() {
    let provider = Provider::start(vec![]).await;
    let workspace = Workspace::new(&provider.base_url);
    let (code, stdout, stderr) = workspace.run(&["doctor"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("loopback/test-model"), "{stdout}");
    assert!(stdout.contains("profile     developer"), "{stdout}");
    assert!(
        stdout.contains("probe .env"),
        "an operator has to be able to see the deny list actually bite: {stdout}"
    );
    assert!(stdout.contains("tool:read_file"), "{stdout}");
}

#[tokio::test]
async fn a_readonly_profile_does_not_offer_the_write_tool() {
    let provider = Provider::start(vec![]).await;
    let workspace = Workspace::new(&provider.base_url);
    let (code, stdout, _stderr) = workspace.run(&["--profile", "readonly", "doctor"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("tool:read_file"), "{stdout}");
    assert!(
        !stdout.contains("tool:write_file"),
        "a tool that is never registered is a tool the model cannot call: {stdout}"
    );
}

#[tokio::test]
async fn doctor_prints_what_each_plugin_actually_registered() {
    // The registrations come from the loader's guard, not from what a plugin claims, so
    // this is also the check that the guard is in the path at all.
    let provider = Provider::start(vec![]).await;
    let workspace = Workspace::new(&provider.base_url);
    let (code, stdout, stderr) = workspace.run(&["doctor"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");

    assert!(
        stdout.contains("ACTIVE"),
        "a committed batch is ACTIVE: {stdout}"
    );
    assert!(stdout.contains("rivet.context-builtin"), "{stdout}");
    assert!(stdout.contains("+ context:system"), "{stdout}");
    assert!(stdout.contains("+ model:loopback/test-model"), "{stdout}");
    assert!(stdout.contains("+ tool:write_file"), "{stdout}");
    assert!(
        !stdout.contains("reported"),
        "no plugin should be overstating what it registered: {stdout}"
    );
}

#[tokio::test]
async fn a_readonly_profile_leaves_write_file_unregistered() {
    // DoD 3 end to end: the profile meets `fs_write` away, the plugin sees the narrowed
    // grant and never registers the tool, so `doctor` cannot print it.
    let provider = Provider::start(vec![]).await;
    let workspace = Workspace::new(&provider.base_url);

    let (code, stdout, stderr) = workspace.run(&["--profile", "developer", "doctor"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("+ tool:write_file"), "{stdout}");

    let (code, stdout, stderr) = workspace.run(&["--profile", "readonly", "doctor"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("+ tool:read_file"), "{stdout}");
    assert!(
        !stdout.contains("tool:write_file"),
        "the plugin must not have registered it at all: {stdout}"
    );
}

#[tokio::test]
async fn with_no_config_file_every_plugin_the_build_provides_is_loaded() {
    // An absent `[plugins].enabled` means "everything this build provides". Every other
    // end-to-end test writes an explicit list, so nothing else here would notice if the
    // loader silently resolved the default to nothing -- and the symptom would be
    // `rivet "explain this repo"` quietly running with no model and no tools.
    let dir = tempfile::tempdir().unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_rivet"))
        .arg("doctor")
        .current_dir(dir.path())
        // The default `api_key_env`, since there is no file to point somewhere else.
        .env("OPENAI_API_KEY", "not-a-real-key")
        .env("RUST_LOG", "warn")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .expect("spawn rivet");
    let stdout = String::from_utf8_lossy(&output.stdout);

    for registration in [
        "+ model:deepseek/deepseek-chat",
        "+ tool:read_file",
        "+ tool:write_file",
        "+ context:system",
        "+ context:workspace",
    ] {
        assert!(
            stdout.contains(registration),
            "{registration} missing: {stdout}"
        );
    }
}

#[tokio::test]
async fn session_list_and_show_read_the_durable_log() {
    let provider = Provider::start(vec![sse_text("hello there")]).await;
    let workspace = Workspace::new(&provider.base_url);
    workspace.run(&["say hello"]).await;

    let (code, stdout, stderr) = workspace.run(&["session", "list"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("say hello"), "{stdout}");

    let id = workspace.session_id().expect("a session");
    let (code, stdout, stderr) = workspace.run(&["session", "show", &id, "--json"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.len() >= 5, "{stdout}");
    for line in lines {
        let event: serde_json::Value = serde_json::from_str(line).expect("durable events are JSON");
        assert!(event["seq"].is_u64(), "{event}");
    }
}

#[tokio::test]
async fn resuming_a_finished_session_is_refused() {
    let provider = Provider::start(vec![sse_text("all done")]).await;
    let workspace = Workspace::new(&provider.base_url);
    workspace.run(&["say hello"]).await;

    let id = workspace.session_id().expect("a session");
    let (code, _stdout, stderr) = workspace.run(&["resume", &id]).await;
    assert_eq!(code, 2);
    assert!(
        stderr.contains("already finished"),
        "re-asking the same question spends tokens for nothing: {stderr}"
    );
}

#[tokio::test]
async fn resuming_a_session_from_another_workspace_is_refused() {
    // `session.created` records the workspace root precisely so this can be caught.
    // Continuing would run another repository's tool calls against this tree.
    let provider = Provider::start(vec![sse_text("hi")]).await;
    let original = Workspace::new(&provider.base_url);
    original.run(&["say hi"]).await;
    let id = original.session_id().expect("a session");

    let elsewhere = Workspace::new(&provider.base_url);
    std::fs::create_dir_all(elsewhere.sessions_dir()).unwrap();
    let copied = elsewhere.sessions_dir().join(&id);
    std::fs::create_dir_all(&copied).unwrap();
    std::fs::copy(original.session_log().unwrap(), copied.join("log.jsonl")).unwrap();

    let (code, _stdout, stderr) = elsewhere.run(&["resume", &id]).await;
    assert_eq!(code, 2);
    assert!(stderr.contains("was created in"), "{stderr}");
}

/// A `kill -9` between `tool.called` and `tool.completed`, and what resume does with it.
///
/// The interruption is made deterministic with a named pipe: opening one for reading blocks
/// until a writer appears, and nothing ever writes. So the run is guaranteed to be sitting
/// inside the tool, with `tool.called` already fsynced, when the signal arrives.
#[cfg(unix)]
#[tokio::test]
async fn a_killed_run_is_closed_with_a_synthetic_result_and_resumes() {
    let provider = Provider::start(vec![
        sse_tool_call("read_file", &serde_json::json!({ "path": "pipe" })),
        sse_text("The read was interrupted, so I stopped."),
    ])
    .await;
    let workspace = Workspace::new(&provider.base_url);

    let made = std::process::Command::new("mkfifo")
        .arg(workspace.path().join("pipe"))
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed");

    let mut child = workspace
        .command(&["read the pipe"])
        .spawn()
        .expect("spawn rivet");

    assert!(
        workspace
            .wait_for_event("tool.called", Duration::from_secs(10))
            .await,
        "the run never reached the tool: {:?}",
        workspace.session_events()
    );
    assert!(
        !workspace
            .session_events()
            .contains(&"tool.completed".to_string()),
        "the tool must still be blocked on the pipe"
    );

    // SIGKILL: no chance to run any cleanup, which is the point.
    child.start_kill().expect("kill");
    let _ = child.wait().await;

    let events = workspace.session_events();
    assert!(events.contains(&"tool.called".to_string()));
    assert!(!events.contains(&"tool.completed".to_string()));

    let id = workspace.session_id().expect("a session");
    let (code, _stdout, stderr) = workspace.run(&["resume", &id]).await;
    assert_eq!(code, 0, "stderr: {stderr}");

    // Exactly one synthetic close was written...
    let closes = workspace
        .session_events()
        .iter()
        .filter(|e| *e == "tool.completed")
        .count();
    assert_eq!(closes, 1, "{:?}", workspace.session_events());

    // ...and the resumed request carried it, in the right place.
    let requests = provider.requests().await;
    let last = requests.last().expect("a resumed request");
    let results = tool_results(last);
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(
        results[0].contains("stopped before this tool finished"),
        "{results:?}"
    );

    let messages = last["messages"].as_array().unwrap();
    let call_index = messages
        .iter()
        .position(|m| m["tool_calls"].is_array())
        .expect("an assistant message with tool calls");
    assert_eq!(
        messages[call_index + 1]["role"],
        "tool",
        "the result has to come straight after the call, or the provider rejects it"
    );
}

/// Ctrl-C at a terminal: the one part of the cancellation story that only a real signal
/// can exercise.
///
/// The offline tests cover everything downstream of the run token — a dropped stream
/// disconnects the request, a cooperative tool stops, an uncooperative one is abandoned
/// inside the budget. What they cannot reach is `signals::install` actually wiring
/// `tokio::signal::ctrl_c()` to `cancel.cancel()`, and the process then leaving with 130
/// instead of having to be killed.
///
/// Parked on a named pipe like the SIGKILL test above, so the signal is guaranteed to
/// arrive while a tool is in flight — and an uncooperative tool at that, which is the
/// harder of the two shutdown paths.
#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_stops_an_in_flight_tool_and_exits_130() {
    let provider = Provider::start(vec![
        sse_tool_call("read_file", &serde_json::json!({ "path": "pipe" })),
        sse_text("never reached: the run is cancelled before a second turn"),
    ])
    .await;
    let workspace = Workspace::new(&provider.base_url);

    let made = std::process::Command::new("mkfifo")
        .arg(workspace.path().join("pipe"))
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed");

    let mut child = workspace
        .command(&["read the pipe"])
        .spawn()
        .expect("spawn rivet");
    let pid = child.id().expect("the child must still be running");

    assert!(
        workspace
            .wait_for_event("tool.called", Duration::from_secs(10))
            .await,
        "the run never reached the tool: {:?}",
        workspace.session_events()
    );
    assert!(
        !workspace
            .session_events()
            .contains(&"tool.completed".to_string()),
        "the tool must still be blocked on the pipe when the signal lands"
    );

    // `kill -INT` rather than `libc::kill`: this workspace forbids `unsafe_code`.
    let signalled = std::process::Command::new("kill")
        .arg("-INT")
        .arg(pid.to_string())
        .status()
        .expect("run kill");
    assert!(signalled.success(), "could not signal the child");

    let sent_at = Instant::now();
    let status = tokio::time::timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("SIGINT must end the run; a hang here means the token is not wired")
        .expect("wait for the child");
    let took = sent_at.elapsed();

    assert_eq!(
        status.code(),
        Some(130),
        "a cancelled run exits 128 + SIGINT, so a script can tell it from a failure"
    );
    // DoD 3's actual number, measured from the signal rather than reasoned about.
    assert!(
        took < Duration::from_secs(5),
        "the run took {took:?} to stop after Ctrl-C; the budget is 5s"
    );

    // The log is the record: the run recorded that it stopped rather than vanishing.
    let events = workspace.session_events();
    assert!(events.contains(&"tool.called".to_string()), "{events:?}");
    assert!(
        events.iter().any(|e| e == "run.completed"),
        "a cancelled run still closes its own log: {events:?}"
    );
}
