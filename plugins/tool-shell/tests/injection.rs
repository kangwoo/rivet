//! No model-supplied string reaches `sh` in a position it would read as an option.
//!
//! The same walk `tool-git` runs, over this plugin's own schema, and for the same reason:
//! `command` is safe today because `-c` consumes it, and that safety is a fact about how it
//! was written rather than about what it is. A second string argument added later — an
//! `interpreter`, a `stdin_file` — would be safe only if whoever added it happened to pick
//! the right door. This test is what makes that not a matter of happening to.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::id::{AgentId, RunId, SessionId, ToolCallId};
use rivet_core::sandbox::{ExecOutput, ExecSpec};
use rivet_core::tool::{Tool, ToolContext, ToolContextData, ToolHost};
use rivet_core::workspace::Workspace;
use rivet_runtime::argv::option_exposure;
use rivet_tool_shell::Shell;

/// Option-shaped. `sh --version` is harmless; `sh -c` reading its command as an option is
/// not the failure here — the failure is a *later* argument landing free-standing.
const INJECTED: &str = "--output=escaped-by-the-model";

#[derive(Debug)]
struct SpyHost {
    seen: Mutex<Vec<ExecSpec>>,
}

#[async_trait]
impl ToolHost for SpyHost {
    fn progress(&self, _message: &str) {}

    fn is_cancelled(&self) -> bool {
        false
    }

    async fn cancelled(&self) {
        std::future::pending::<()>().await;
    }

    async fn exec(&self, spec: ExecSpec) -> rivet_core::Result<ExecOutput> {
        self.seen.lock().unwrap().push(spec);
        Ok(ExecOutput {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            truncated: false,
            duration_ms: 1,
        })
    }
}

fn ctx(host: Arc<SpyHost>) -> ToolContext {
    ToolContext::new(
        ToolContextData {
            session_id: SessionId::new(),
            agent_id: AgentId::new(),
            run_id: RunId::new(),
            call_id: ToolCallId::new(),
            workspace: Workspace::new(std::path::PathBuf::from("/repo")),
            permissions: PermissionSet::new([
                Permission::ProcessSpawn,
                Permission::FsWrite(FsScope::Workspace),
            ]),
            timeout_ms: Some(30_000),
            max_output_bytes: Some(4_096),
        },
        host,
    )
}

#[tokio::test]
async fn no_declared_argument_can_become_an_option() {
    let spec = Shell.spec();
    let properties = spec.input_schema["properties"]
        .as_object()
        .expect("a declared object schema");
    let strings: Vec<&String> = properties
        .iter()
        .filter(|(_, schema)| schema["type"] == "string")
        .map(|(key, _)| key)
        .collect();

    for key in &strings {
        let host = Arc::new(SpyHost {
            seen: Mutex::new(Vec::new()),
        });
        let mut input = serde_json::Map::new();
        // `command` is required, so it is always present; the key under test overwrites it
        // when that key *is* `command`.
        input.insert("command".to_string(), serde_json::json!("true"));
        input.insert((*key).clone(), serde_json::json!(INJECTED));

        let outcome = Shell
            .execute(ctx(host.clone()), serde_json::Value::Object(input))
            .await;

        let seen = host.seen.lock().unwrap();
        if outcome.is_err() {
            // Refused before anything ran.
            assert!(
                seen.is_empty(),
                "{key}: refused, but `sh` was started anyway"
            );
        } else {
            // Allowed through, so it must be somewhere `sh` reads as a value: bound to the
            // option before it, or not in argv at all.
            let ran = seen.first().expect("one exec").clone();
            assert_eq!(
                option_exposure(&ran.args, INJECTED),
                None,
                "{key}: reached argv where `sh` would parse it as an option: {:?}",
                ran.args
            );
        }
    }

    let mut found: Vec<&str> = strings.iter().map(|k| k.as_str()).collect();
    found.sort_unstable();
    assert_eq!(
        found,
        ["command", "cwd"],
        "the schema walk found a different set of string arguments than `shell` declares"
    );
}

#[tokio::test]
async fn a_command_that_begins_with_a_dash_is_still_a_command() {
    // `-c` consumes the next argv entry verbatim, so this was never dangerous. Refusing it
    // would break `shell{command: "-n foo"}` for no gain, and the model would have no way to
    // tell a real refusal from an arbitrary one.
    let host = Arc::new(SpyHost {
        seen: Mutex::new(Vec::new()),
    });
    Shell
        .execute(
            ctx(host.clone()),
            serde_json::json!({ "command": "--version" }),
        )
        .await
        .expect("a command bound to `-c` is a command");

    let spec = host.seen.lock().unwrap().first().expect("one exec").clone();
    assert_eq!(spec.program, "sh");
    assert_eq!(spec.args, ["-c", "--version"]);
    assert_eq!(option_exposure(&spec.args, "--version"), None);
}

#[tokio::test]
async fn the_working_directory_never_reaches_argv() {
    // `cwd` travels on `ExecSpec::cwd`, where the sandbox resolves it through `fsguard`.
    // It is not an argument, and a change that made it one would be caught by the walk
    // above rather than by anyone remembering this.
    let host = Arc::new(SpyHost {
        seen: Mutex::new(Vec::new()),
    });
    Shell
        .execute(
            ctx(host.clone()),
            serde_json::json!({ "command": "ls", "cwd": "src" }),
        )
        .await
        .expect("the spy answers");

    let spec = host.seen.lock().unwrap().first().expect("one exec").clone();
    assert_eq!(spec.cwd, Some(std::path::PathBuf::from("src")));
    assert!(
        !spec.args.iter().any(|a| a == "src"),
        "the working directory is not an argument: {:?}",
        spec.args
    );
}
