//! A real workspace on disk and a prepared handle, for the tests that need a real process.

#![allow(dead_code)]

use rivet_core::capability::{Permission, PermissionSet};
use rivet_core::sandbox::{Sandbox, SandboxHandle, SandboxRequest};
use rivet_sandbox_local::{LocalSandbox, Settings};

/// A temporary workspace and a handle prepared against it.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub handle: Box<dyn SandboxHandle>,
}

impl Fixture {
    /// A handle whose environment carries only `names` from the host.
    pub async fn with_passthrough(names: &[&str]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = rivet_runtime::workspace::open(dir.path(), Vec::new()).expect("workspace");
        let handle = LocalSandbox::new(Settings {
            env_passthrough: names.iter().map(|s| (*s).to_string()).collect(),
        })
        .prepare(SandboxRequest {
            workspace,
            permissions: PermissionSet::new([Permission::ProcessSpawn]),
            options: serde_json::Map::new(),
        })
        .await
        .expect("a grant with process_spawn prepares");
        Self { dir, handle }
    }

    /// The usual case: `PATH` and `HOME`, which is what makes `sh` findable.
    pub async fn new() -> Self {
        Self::with_passthrough(&["PATH", "HOME"]).await
    }

    pub fn path(&self) -> &std::path::Path {
        self.dir.path()
    }
}
