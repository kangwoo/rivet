//! What `shell` hands to the host, and what it does with what comes back.
//!
//! The host is a spy rather than a real sandbox: what is under test here is the *tool's*
//! half of the contract — that it goes through `ctx.host.exec`, that the argv says which
//! shell, and that the budget only ever narrows. The sandbox's half has its own tests.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::id::{AgentId, RunId, SessionId, ToolCallId};
use rivet_core::sandbox::{ExecOutput, ExecSpec};
use rivet_core::tool::{Tool, ToolContext, ToolContextData, ToolHost};
use rivet_core::workspace::Workspace;
use rivet_tool_shell::Shell;

/// Records every `exec` and answers with a fixed output.
#[derive(Debug)]
struct SpyHost {
    seen: Mutex<Vec<ExecSpec>>,
    answer: ExecOutput,
}

impl SpyHost {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            answer: ExecOutput {
                exit_code: Some(0),
                stdout: "ok".into(),
                stderr: String::new(),
                timed_out: false,
                truncated: false,
                duration_ms: 3,
            },
        })
    }

    fn only_call(&self) -> ExecSpec {
        let seen = self.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "expected exactly one exec: {seen:?}");
        seen[0].clone()
    }
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
        Ok(self.answer.clone())
    }
}

fn ctx(host: Arc<SpyHost>, timeout_ms: Option<u64>) -> ToolContext {
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
            timeout_ms,
            max_output_bytes: Some(4_096),
        },
        host,
    )
}

#[tokio::test]
async fn the_tool_never_spawns_directly() {
    // Going through the host is what makes the sandbox, the timeout and the output cap
    // apply at all. A tool that reached for `std::process::Command` would be outside all
    // three while looking like it was inside them.
    let host = SpyHost::new();
    Shell
        .execute(
            ctx(host.clone(), Some(30_000)),
            serde_json::json!({ "command": "cargo test" }),
        )
        .await
        .expect("the spy answers");
    assert_eq!(host.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn sh_minus_c_is_explicit_in_the_argv() {
    let host = SpyHost::new();
    Shell
        .execute(
            ctx(host.clone(), Some(30_000)),
            serde_json::json!({ "command": "echo hi" }),
        )
        .await
        .expect("the spy answers");

    let spec = host.only_call();
    assert_eq!(spec.program, "sh");
    assert_eq!(spec.args, ["-c", "echo hi"]);
}

#[tokio::test]
async fn a_narrower_timeout_wins_over_the_context_budget() {
    let host = SpyHost::new();
    Shell
        .execute(
            ctx(host.clone(), Some(30_000)),
            serde_json::json!({ "command": "true", "timeout_ms": 5_000 }),
        )
        .await
        .expect("the spy answers");
    assert_eq!(host.only_call().timeout_ms, Some(5_000));

    let wider = SpyHost::new();
    Shell
        .execute(
            ctx(wider.clone(), Some(30_000)),
            serde_json::json!({ "command": "true", "timeout_ms": 300_000 }),
        )
        .await
        .expect("the spy answers");
    assert_eq!(
        wider.only_call().timeout_ms,
        Some(30_000),
        "a tool must not be able to widen the run's budget"
    );
}

#[tokio::test]
async fn the_cwd_and_the_output_cap_are_passed_through() {
    let host = SpyHost::new();
    Shell
        .execute(
            ctx(host.clone(), None),
            serde_json::json!({ "command": "ls", "cwd": "src" }),
        )
        .await
        .expect("the spy answers");

    let spec = host.only_call();
    assert_eq!(spec.cwd, Some(std::path::PathBuf::from("src")));
    assert_eq!(spec.max_output_bytes, Some(4_096));
}

#[tokio::test]
async fn a_spec_carries_no_environment_of_its_own() {
    // The sandbox starts from an empty environment and adds what an operator named. A tool
    // that filled `env` here would be routing around that list.
    let host = SpyHost::new();
    Shell
        .execute(
            ctx(host.clone(), None),
            serde_json::json!({ "command": "env" }),
        )
        .await
        .expect("the spy answers");
    assert!(host.only_call().env.is_empty());
}

#[tokio::test]
async fn a_host_refusal_comes_back_as_a_runtime_error() {
    // A missing sandbox provider is not something the model can adapt to by rephrasing, so
    // it is an `Err` rather than a tool result the model is asked to react to.
    #[derive(Debug)]
    struct RefusingHost;

    #[async_trait]
    impl ToolHost for RefusingHost {
        fn progress(&self, _message: &str) {}
        fn is_cancelled(&self) -> bool {
            false
        }
        async fn cancelled(&self) {
            std::future::pending::<()>().await;
        }
        async fn exec(&self, _spec: ExecSpec) -> rivet_core::Result<ExecOutput> {
            Err(rivet_core::Error::not_found(
                "no sandbox provider named `local` is registered",
            ))
        }
    }

    let ctx = ToolContext::new(
        ToolContextData {
            session_id: SessionId::new(),
            agent_id: AgentId::new(),
            run_id: RunId::new(),
            call_id: ToolCallId::new(),
            workspace: Workspace::new(std::path::PathBuf::from("/repo")),
            permissions: PermissionSet::empty(),
            timeout_ms: None,
            max_output_bytes: None,
        },
        Arc::new(RefusingHost),
    );
    let error = Shell
        .execute(ctx, serde_json::json!({ "command": "true" }))
        .await
        .expect_err("a missing provider is a runtime failure");
    assert!(error.message().contains("local"), "{error}");
}

#[test]
fn the_declared_schema_matches_what_the_tool_reads() {
    // Step 3 validates against this schema before the tool sees anything, so a key the tool
    // reads but the schema does not declare would arrive rejected.
    let spec = Shell.spec();
    let properties = spec.input_schema["properties"]
        .as_object()
        .expect("properties");
    for key in ["command", "cwd", "timeout_ms"] {
        assert!(properties.contains_key(key), "{key} is not declared");
    }
    assert_eq!(
        spec.input_schema["additionalProperties"],
        serde_json::json!(false)
    );
    assert_eq!(
        spec.input_schema["required"],
        serde_json::json!(["command"])
    );
}
