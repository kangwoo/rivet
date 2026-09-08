//! A workspace and a live [`ToolContext`] for the filesystem tools.

#![allow(dead_code)]

use std::sync::Arc;

use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::id::{AgentId, RunId, SessionId, ToolCallId};
use rivet_core::sandbox::SandboxRequest;
use rivet_core::tool::{ToolContext, ToolContextData};
use rivet_core::workspace::Workspace;
use rivet_runtime::BroadcastBus;
use rivet_runtime::dispatch::RuntimeToolHost;
use rivet_runtime::sandbox_scope::SandboxScope;
use tokio_util::sync::CancellationToken;

/// The deny list `rivet.example.toml` ships and `docs/security.md` recommends.
pub const DENY: [&str; 5] = [
    ".env",
    ".git/config",
    ".ssh",
    "**/credentials.json",
    "**/*.pem",
];

/// A small repository plus a directory outside it holding something secret.
pub struct Fixture {
    pub inside: tempfile::TempDir,
    pub outside: tempfile::TempDir,
    pub workspace: Workspace,
    pub cancel: CancellationToken,
}

impl Fixture {
    pub fn new() -> Self {
        let inside = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        std::fs::write(outside.path().join("passwd"), "root:x:0:0").unwrap();

        std::fs::create_dir(inside.path().join("src")).unwrap();
        std::fs::write(
            inside.path().join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        std::fs::write(
            inside.path().join("src/lib.rs"),
            "// TODO: write the library\npub fn nothing() {}\n",
        )
        .unwrap();
        std::fs::write(
            inside.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        std::fs::write(inside.path().join(".env"), "TOKEN=secret-do-not-read\n").unwrap();
        std::fs::create_dir(inside.path().join(".git")).unwrap();
        std::fs::write(inside.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();

        let workspace =
            rivet_runtime::workspace::open(inside.path(), DENY.map(String::from)).expect("open");
        Self {
            inside,
            outside,
            workspace,
            cancel: CancellationToken::new(),
        }
    }

    pub fn path(&self, relative: &str) -> std::path::PathBuf {
        self.inside.path().join(relative)
    }

    /// A context wired to a real [`RuntimeToolHost`], so cancellation behaves as it does
    /// in a run rather than as a stub.
    pub fn ctx(&self) -> ToolContext {
        let bus = Arc::new(BroadcastBus::new());
        let session_id = SessionId::new();
        let run_id = RunId::new();
        let call_id = ToolCallId::new();
        ToolContext::new(
            ToolContextData {
                session_id,
                agent_id: AgentId::new(),
                run_id,
                call_id,
                workspace: self.workspace.clone(),
                permissions: PermissionSet::new([
                    Permission::FsRead(FsScope::Workspace),
                    Permission::FsWrite(FsScope::Workspace),
                ]),
                timeout_ms: Some(30_000),
                max_output_bytes: Some(65_536),
            },
            Arc::new(RuntimeToolHost::new(
                bus,
                session_id,
                run_id,
                call_id,
                self.cancel.child_token(),
                // No provider, and none needed: these tools open files, they do not start
                // processes. A missing sandbox is only a failure at `exec`.
                Arc::new(SandboxScope::unconfined(SandboxRequest {
                    workspace: self.workspace.clone(),
                    permissions: PermissionSet::empty(),
                    options: serde_json::Map::new(),
                })),
            )),
        )
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
