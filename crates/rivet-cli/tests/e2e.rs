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
async fn with_no_config_file_every_plugin_the_default_selection_names_is_loaded() {
    // An absent `[plugins].enabled` means "the default selection" -- which is not the same
    // as "the whole catalog" since `catalog::default_selection` narrowed it. Every other
    // end-to-end test writes an explicit list, so nothing else here would notice if the
    // loader silently resolved the default to nothing -- and the symptom would be
    // `rivet "explain this repo"` quietly running with no model and no tools.
    //
    // Renamed from `..._every_plugin_the_build_provides_is_loaded`: the body always
    // asserted about the agent stack, and the old name became false the moment the catalog
    // carried a plugin the default does not enable.
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
    assert!(
        !stdout.contains("subscriber:telemetry.log"),
        "the observation sidecar must not switch itself on in an unconfigured tree: {stdout}"
    );
}

#[tokio::test]
async fn the_telemetry_plugin_is_not_in_the_default_selection() {
    // The other half of the same rule, from the plugin's own side: it is in the catalog, so
    // `rivet plugin list` shows it and `enabled` can name it -- and it is not loaded.
    let provider = Provider::start(vec![sse_text("unused")]).await;
    let workspace = Workspace::new(&provider.base_url);
    std::fs::remove_file(workspace.path().join("rivet.toml")).unwrap();

    let (code, stdout, stderr) = workspace
        .command(&["plugin", "list"])
        .env("OPENAI_API_KEY", "not-a-real-key")
        .output()
        .await
        .map(|o| {
            (
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            )
        })
        .expect("spawn rivet");
    assert_eq!(code, 0, "stderr: {stderr}");

    let row = stdout
        .lines()
        .find(|line| line.contains("rivet.telemetry-log"))
        .unwrap_or_else(|| panic!("telemetry is not in the catalog at all: {stdout}"));
    assert!(
        row.contains("VALIDATED"),
        "it has to be discovered and validated: {row}"
    );
    assert!(
        row.split_whitespace().nth(1) == Some("no"),
        "the ENABLED column has to say `no` with no config file: {row}"
    );
}

#[tokio::test]
async fn the_telemetry_plugin_logs_a_run_when_enabled() {
    // 3.3 end to end: switched on by name, it emits structured records to stderr, and
    // `RIVET_LOG_FORMAT=json` makes them parseable rather than pretty.
    let provider = Provider::start(vec![sse_text("hello")]).await;
    let workspace = Workspace::new(&provider.base_url);
    enable_telemetry(&workspace);

    let output = workspace
        .command(&["say hello"])
        .env("RIVET_LOG_FORMAT", "json")
        .env_remove("RUST_LOG")
        .output()
        .await
        .expect("spawn rivet");
    let stderr = String::from_utf8_lossy(&output.stderr);

    let records: Vec<serde_json::Value> = stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| value["fields"]["message"] == "rivet event")
        .collect();
    assert!(
        !records.is_empty(),
        "the telemetry plugin was enabled and logged nothing: {stderr}"
    );
    for record in &records {
        assert!(
            record["fields"]["topic"].is_string(),
            "every record names its topic: {record}"
        );
    }
    assert!(
        records
            .iter()
            .any(|r| r["fields"]["topic"] == "agent.run.started"),
        "the run itself has to be in there: {stderr}"
    );
    assert!(
        !records
            .iter()
            .any(|r| r["fields"]["topic"] == "agent.text.delta"),
        "the conversation is off by default, so it is not even subscribed to: {stderr}"
    );
}

/// Rewrite the workspace's config to enable the telemetry plugin alongside the agent stack.
fn enable_telemetry(workspace: &Workspace) {
    let path = workspace.path().join("rivet.toml");
    let text = std::fs::read_to_string(&path).unwrap().replace(
        "enabled = [\"rivet.model-openai\", \"rivet.tool-filesystem\"]",
        "enabled = [\"rivet.model-openai\", \"rivet.tool-filesystem\", \"rivet.telemetry-log\"]",
    );
    std::fs::write(&path, text).unwrap();
}

// --- DoD 6: `--jsonl` is observability, and not a session export -----------------------------

/// Phase 3 `DoD` 6, first half.
///
/// Before this phase the observer was attached *after* `catalog::load`, so `runtime.started`
/// and the whole plugin lifecycle happened with nobody listening, and the tail of the stream
/// was cut by `sleep(20 ms); abort()`. Both ends are asserted here.
#[tokio::test]
async fn jsonl_carries_the_whole_lifecycle_not_just_the_answer() {
    let provider = Provider::start(vec![
        sse_tool_call("read_file", &serde_json::json!({"path": "src/main.rs"})),
        sse_text("it prints a marker"),
    ])
    .await;
    let workspace = Workspace::new(&provider.base_url);

    let (code, stdout, stderr) = workspace.run(&["--jsonl", "read the file"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");

    let topics: Vec<String> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|value| topic_of(&value))
        .collect();

    assert_eq!(
        topics.first().map(String::as_str),
        Some("runtime.started"),
        "the stream has to start with the runtime announcing itself: {topics:?}"
    );
    for expected in [
        "runtime.started",
        "plugin.discovered",
        "plugin.loaded",
        "agent.run.started",
        "tool.execute.started",
        "tool.execute.completed",
        "agent.run.completed",
        "runtime.shutting_down",
        "plugin.unloaded",
    ] {
        assert!(
            topics.iter().any(|t| t == expected),
            "`{expected}` missing from the stream: {topics:?}"
        );
    }

    let shutting = topics.iter().position(|t| t == "runtime.shutting_down");
    let unloaded = topics.iter().position(|t| t == "plugin.unloaded");
    assert!(
        shutting < unloaded,
        "shutting_down announces the teardown, so it comes first: {topics:?}"
    );
}

/// Phase 3 `DoD` 6, second half — the part `docs/plan.md` rewrote the `DoD` for.
///
/// The bus is lossy, so its stream can never be a faithful record. The durable facts live
/// in the session log and are reachable through `rivet session show --json`. This asserts
/// the two are different in the way that matters: the durable one carries the conversation
/// and a sequence number, and the bus stream carries neither.
#[tokio::test]
async fn the_jsonl_stream_is_not_a_session_export() {
    let provider = Provider::start(vec![sse_text("hello there")]).await;
    let workspace = Workspace::new(&provider.base_url);

    let (code, stream, stderr) = workspace.run(&["--jsonl", "say hello"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");

    let bus: Vec<serde_json::Value> = stream
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(!bus.is_empty(), "no bus events: {stream}");
    for event in &bus {
        assert!(
            event["seq"].is_null(),
            "a bus event has no sequence number; nothing orders it durably: {event}"
        );
    }

    let session = workspace.session_id().expect("a session");
    let (code, durable, stderr) = workspace
        .run(&["session", "show", &session, "--json"])
        .await;
    assert_eq!(code, 0, "stderr: {stderr}");

    let stored: Vec<serde_json::Value> = durable
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let durable_types: Vec<&str> = stored
        .iter()
        .filter_map(|e| e["event"]["type"].as_str())
        .collect();
    for expected in ["user.message", "assistant.message"] {
        assert!(
            durable_types.contains(&expected),
            "`{expected}` is a durable fact and has to be in the session log: {durable_types:?}"
        );
    }
    assert!(
        stored.iter().all(|e| e["seq"].is_number()),
        "every durable event is sequenced: {durable}"
    );

    // And the bus stream carries none of the three.
    let bus_topics: Vec<String> = bus.iter().filter_map(topic_of).collect();
    for absent in ["user.message", "assistant.message"] {
        assert!(
            !bus_topics.iter().any(|t| t == absent),
            "`{absent}` is a durable fact and must not be read off the lossy bus: {bus_topics:?}"
        );
    }
}

/// The topic of a serialized envelope, rebuilt from its tagged payload.
fn topic_of(event: &serde_json::Value) -> Option<String> {
    let payload = event.get("payload")?;
    let event_name = payload.get("event")?.as_str()?;
    let kind = payload.get("kind")?.as_str()?;
    // The wire form is `{event: "tool", kind: "started"}`; the topic is what
    // `Event::topic` returns, so this maps the two-field form onto it.
    Some(match (event_name, kind) {
        ("agent", "run_started") => "agent.run.started".into(),
        ("agent", "turn_started") => "agent.turn.started".into(),
        ("agent", "request_started") => "agent.request.started".into(),
        ("agent", "text_delta") => "agent.text.delta".into(),
        ("agent", "request_completed") => "agent.request.completed".into(),
        ("agent", "request_failed") => "agent.request.failed".into(),
        ("agent", "turn_completed") => "agent.turn.completed".into(),
        ("agent", "run_completed") => "agent.run.completed".into(),
        ("tool", "requested") => "tool.requested".into(),
        ("tool", "started") => "tool.execute.started".into(),
        ("tool", "progress") => "tool.execute.progress".into(),
        ("tool", "completed") => "tool.execute.completed".into(),
        ("tool", "blocked") => "tool.blocked".into(),
        ("plugin", "discovered") => "plugin.discovered".into(),
        ("plugin", "loaded") => "plugin.loaded".into(),
        ("plugin", "load_failed") => "plugin.load.failed".into(),
        ("plugin", "unloaded") => "plugin.unloaded".into(),
        ("runtime", "started") => "runtime.started".into(),
        ("runtime", "shutting_down") => "runtime.shutting_down".into(),
        ("runtime", "subscriber_lagged") => "runtime.subscriber.lagged".into(),
        (family, kind) => format!("{family}.{kind}"),
    })
}

/// The stream has to carry the failure, not just the exit code.
///
/// Nothing asserted this: every other `--jsonl` test runs a successful run, and the failing
/// path is the one an operator actually reads the stream for. `catalog::load` publishes the
/// discoveries, the refusal, and the rollback's `plugin.unloaded` lines, and only then
/// returns `Err`.
///
/// Worth being exact about what this does and does not catch. Before `Watching::finish`,
/// that `Err` left `start` through a `?` and `Watching` was dropped — reaching
/// [`rivet_runtime::Observer`]'s `Drop`, which **aborts** the pump instead of draining it.
/// The lines still arrived, every time, on every machine tried: `unload_all` awaits between
/// the last publish and the drop, and a multi-threaded runtime hands the pump a worker long
/// before then. So this test passes either way, and it is not a regression test for that
/// abort. What it pins is the property — the stream ends where the run does — which used to
/// hold by scheduler luck and now holds by construction, the same trade `drain_within` made
/// against `sleep(20 ms); abort()`.
#[tokio::test]
async fn jsonl_carries_a_failed_plugin_load_and_not_just_the_exit_code() {
    let provider = Provider::start(vec![sse_text("unused")]).await;
    let workspace = Workspace::new(&provider.base_url);
    enable_telemetry(&workspace);
    // A written `topics` list is a promise, and `agent.text.` is exactly what a narrowed
    // profile withholds -- so this plugin refuses to load rather than log less than it was
    // told to. Any load failure would do; this is the one Phase 3 shipped.
    let path = workspace.path().join("rivet.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!("{config}\n[plugins.\"rivet.telemetry-log\"]\ntopics = [\"agent.text.\"]\n"),
    )
    .unwrap();

    let (code, stdout, stderr) = workspace
        .run(&["--jsonl", "--profile", "readonly", "say hello"])
        .await;
    assert_ne!(code, 0, "a plugin refused to load, so the run has to fail");

    let topics: Vec<String> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|value| topic_of(&value))
        .collect();
    assert!(
        topics.iter().any(|t| t == "plugin.load.failed"),
        "the stream is missing the one line that explains the exit: \
         {topics:?} (stderr: {stderr})"
    );
    assert!(
        topics.iter().any(|t| t == "plugin.unloaded"),
        "the rollback is the tail of this stream, and the last thing published before the \
         error leaves: {topics:?}"
    );
}

#[tokio::test]
async fn plugin_list_does_not_tell_you_to_enable_what_you_already_enabled() {
    // The footer names what is off and what to do about it. It compared against
    // `catalog::default_selection()` rather than the resolved `enabled` set -- and those two
    // differ exactly when somebody has written a config -- so an operator who had put the
    // telemetry plugin in `[plugins].enabled` got a row reading `ENABLED yes` and, under it,
    // an instruction to go and put it in `[plugins].enabled`.
    let provider = Provider::start(vec![]).await;
    let workspace = Workspace::new(&provider.base_url);
    workspace.enable_plugins(&[
        "rivet.model-openai",
        "rivet.tool-filesystem",
        "rivet.telemetry-log",
    ]);

    let (code, stdout, stderr) = workspace.run(&["plugin", "list"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");

    let footer = stdout
        .lines()
        .find(|line| line.contains("[plugins].enabled"))
        .unwrap_or("");
    assert!(
        !footer.contains("rivet.telemetry-log"),
        "it is enabled in this config: {stdout}"
    );

    // And the other direction, so this is not passing by never printing a footer at all: the
    // default selection leaves the telemetry plugin out, so an unconfigured tree is told.
    let bare = Workspace::new(&provider.base_url);
    let (code, stdout, stderr) = bare.run(&["plugin", "list"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout
            .lines()
            .any(|line| line.contains("[plugins].enabled") && line.contains("rivet.telemetry-log")),
        "a plugin nothing enabled has to say how to turn it on: {stdout}"
    );
}

#[tokio::test]
async fn tui_refuses_a_pipe() {
    // Raw mode on a pipe leaves no terminal to put back, and the symptom shows up later as
    // a shell that stopped echoing. So it is refused before raw mode, as exit code 2.
    let provider = Provider::start(vec![sse_text("unused")]).await;
    let workspace = Workspace::new(&provider.base_url);

    let (code, _stdout, stderr) = workspace.run(&["--tui", "hello"]).await;
    assert_eq!(code, 2, "a setup problem is exit code 2: {stderr}");
    assert!(stderr.contains("needs a terminal"), "{stderr}");
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
