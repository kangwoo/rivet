//! The sandbox contract: *where* an action runs.
//!
//! Policy answers "may this happen"; sandbox answers "under what confinement". They are
//! separate because the same `git push` may be allowed-and-unconfined on a laptop and
//! allowed-but-containerized in CI, and neither decision should be able to silently
//! change the other.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::capability::PermissionSet;
use crate::id::SandboxId;
use crate::workspace::Workspace;

/// A process to run inside a sandbox.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecSpec {
    /// The program. Never a shell string — the caller chooses the shell explicitly by
    /// setting `program` to `sh` and passing `-c`, which keeps the injection surface
    /// visible in the session log instead of hidden in string concatenation.
    pub program: String,
    pub args: Vec<String>,
    /// Working directory, relative to the workspace root.
    pub cwd: Option<PathBuf>,
    /// Environment. The sandbox starts from an empty environment and adds only these, so
    /// a leaked `AWS_SECRET_ACCESS_KEY` requires someone to have typed it.
    ///
    /// A provider may add names an operator listed for it — `sandbox-local` reads
    /// `[plugins."rivet.sandbox-local"] env_passthrough`, a list of variable *names*
    /// copied from the host environment. That table is where "someone has to have typed
    /// it" actually happens; the values never appear in a config file or a log.
    pub env: BTreeMap<String, String>,
    pub stdin: Option<String>,
    pub timeout_ms: Option<u64>,
    /// Cap on captured output. Beyond this the sandbox truncates and reports it.
    pub max_output_bytes: Option<u64>,
}

impl ExecSpec {
    pub fn new(program: impl Into<String>, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().collect(),
            cwd: None,
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: None,
            max_output_bytes: None,
        }
    }
}

/// The result of a sandboxed process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecOutput {
    /// `None` when the process was killed by a signal or a timeout.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub truncated: bool,
    pub duration_ms: u64,
}

impl ExecOutput {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }
}

/// What a sandbox is asked to prepare.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxRequest {
    pub workspace: Workspace,
    /// The grant the confinement must enforce.
    pub permissions: PermissionSet,
    /// Provider-specific settings (image name, memory limit, seccomp profile).
    #[serde(default)]
    pub options: serde_json::Map<String, serde_json::Value>,
}

/// A prepared environment.
///
/// # Teardown is the runtime's job, not `Drop`'s
///
/// Releasing a sandbox means awaiting something — killing a process group, stopping a
/// container, unmounting a filesystem — and `Drop` cannot await. A contract that said
/// "dropping releases resources" would therefore be a promise Rust cannot keep, and the
/// containers would leak.
///
/// So the runtime **must** call [`SandboxHandle::teardown`] on every path, including
/// cancellation and panic-unwind, before dropping the handle. Implementations should treat
/// a `Drop` without a prior `teardown` as a bug worth logging loudly.
#[async_trait]
pub trait SandboxHandle: Send + Sync + fmt::Debug {
    fn id(&self) -> SandboxId;

    /// Run a process to completion.
    ///
    /// Must honor `cancel`: on cancellation the process tree is killed, not merely
    /// detached. An orphaned `cargo build` after Ctrl-C is a correctness bug, not a
    /// cosmetic one.
    async fn exec(&self, spec: ExecSpec, cancel: CancellationToken) -> crate::Result<ExecOutput>;

    /// Release the environment. Idempotent.
    async fn teardown(&self) -> crate::Result<()>;
}

/// What confinement a provider actually delivers.
///
/// The runtime surfaces this so an operator is never misled about what they are getting:
/// `sandbox-local` reports `network_isolation: false`, and the UI can say so.
/// A flag struct: the fields are independent guarantees, not variants of one choice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct SandboxGuarantees {
    pub filesystem_isolation: bool,
    pub network_isolation: bool,
    pub process_isolation: bool,
    /// Whether the workspace is copied (changes need syncing back) or bind-mounted.
    pub copies_workspace: bool,
}

/// A sandbox provider.
#[async_trait]
pub trait Sandbox: Send + Sync + fmt::Debug {
    /// Registered name, referenced from config as `[sandbox] provider = "..."`.
    fn name(&self) -> &str;

    /// Honest statement of what this provider enforces.
    fn guarantees(&self) -> SandboxGuarantees;

    async fn prepare(&self, request: SandboxRequest) -> crate::Result<Box<dyn SandboxHandle>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timed_out_processes_are_never_successful() {
        let output = ExecOutput {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            truncated: false,
            duration_ms: 30_000,
        };
        assert!(!output.succeeded(), "a timeout must not read as success");
    }

    #[test]
    fn exec_spec_starts_with_an_empty_environment() {
        let spec = ExecSpec::new("cargo", ["test".to_string()]);
        assert!(
            spec.env.is_empty(),
            "env must be opt-in, never inherited by default"
        );
    }
}
