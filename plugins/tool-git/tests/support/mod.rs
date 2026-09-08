//! A workspace, and a host that records what `git` would have been asked to run.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::id::{AgentId, RunId, SessionId, ToolCallId};
use rivet_core::sandbox::{ExecOutput, ExecSpec};
use rivet_core::tool::{ToolContext, ToolContextData, ToolHost};
use rivet_core::workspace::Workspace;

/// Records every `exec` and answers with a fixed output.
///
/// A spy rather than a real `git`: what is under test is the argv these tools build and the
/// path checking they do before building it. Whether `git` itself works is not this crate's
/// claim.
#[derive(Debug)]
pub struct SpyHost {
    seen: Mutex<Vec<ExecSpec>>,
    answer: Mutex<ExecOutput>,
}

impl SpyHost {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            answer: Mutex::new(ExecOutput {
                exit_code: Some(0),
                stdout: "## main".into(),
                stderr: String::new(),
                timed_out: false,
                truncated: false,
                duration_ms: 3,
            }),
        })
    }

    pub fn answers_with(&self, output: ExecOutput) {
        *self.answer.lock().unwrap() = output;
    }

    /// The one command that was run, or a panic naming what was.
    pub fn only_call(&self) -> ExecSpec {
        let seen = self.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "expected exactly one exec: {seen:?}");
        seen[0].clone()
    }

    pub fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
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
        Ok(self.answer.lock().unwrap().clone())
    }
}

/// A temporary workspace and a context wired to a [`SpyHost`].
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub workspace: Workspace,
    pub host: Arc<SpyHost>,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").expect("write");
        let workspace = rivet_runtime::workspace::open(dir.path(), Vec::new()).expect("workspace");
        Self {
            dir,
            workspace,
            host: SpyHost::new(),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    pub fn ctx(&self) -> ToolContext {
        ToolContext::new(
            ToolContextData {
                session_id: SessionId::new(),
                agent_id: AgentId::new(),
                run_id: RunId::new(),
                call_id: ToolCallId::new(),
                workspace: self.workspace.clone(),
                permissions: PermissionSet::new([
                    Permission::ProcessSpawn,
                    Permission::FsRead(FsScope::Workspace),
                    Permission::FsWrite(FsScope::Workspace),
                ]),
                timeout_ms: Some(30_000),
                max_output_bytes: Some(65_536),
            },
            self.host.clone(),
        )
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
