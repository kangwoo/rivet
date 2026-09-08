//! `shell`: one command, run through whatever confinement the policy chain chose.
//!
//! # It never spawns anything itself
//!
//! Every process goes through `ctx.host.exec`. That is not a style preference: the sandbox,
//! the timeout and the output cap are applied there and nowhere else, so a tool that reached
//! for `std::process::Command` would be running outside all three while looking like it was
//! inside them.
//!
//! # `sh -c` is in the argv, on purpose
//!
//! [`rivet_core::sandbox::ExecSpec`] takes a program and arguments, never a command line, so
//! choosing a shell is an explicit act: `program = "sh"`, `args = ["-c", command]`. The
//! injection surface is then visible in the session log as argv rather than hidden inside a
//! string somebody concatenated.
//!
//! The pairing goes through [`rivet_runtime::argv::Argv`] rather than being written out, and
//! that is not decoration. `-c` consumes the next argv entry verbatim, which is exactly why
//! a command beginning with `-` is a command rather than an option — but "this value is
//! bound to that option" is a fact about *two* pushes, and two pushes can drift apart.
//! `Argv::option` makes it one call, and it takes a [`rivet_runtime::argv::Consuming`]
//! rather than any string, so "the option really does swallow what follows" is checked where
//! that constant is declared instead of here. The other reason is that `Argv` has no `push`: a second argument added here later
//! cannot land free-standing without someone choosing a door, and `tool-git`'s `rev` is what
//! happens when that choice is available to skip.
//!
//! # Who gets a shell
//!
//! `shell` registers only when the effective grant holds **both** `process_spawn` and
//! `fs_write(workspace)`. That is `developer` and `ci` — and it is not an arbitrary pairing:
//! `docs/security.md` §3 notes that write access to `.git/config` is shell access, which
//! read backwards means shell access *is* write access. A profile that has one and not the
//! other is a profile whose boundary would be a fiction.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
use rivet_core::tool::{Tool, ToolAnnotations, ToolContext, ToolResult, ToolSpec};
use rivet_runtime::argv::{Argv, Consuming};

/// The plugin id this crate registers under.
pub const PLUGIN_ID: &str = "rivet.tool-shell";

/// This crate's `rivet-plugin.toml`, for a host catalog to hand to the loader.
pub const MANIFEST_TOML: &str = include_str!("../rivet-plugin.toml");

/// The largest `timeout_ms` the schema accepts. Ten minutes: past that a command is a job,
/// and jobs are Phase 5.
pub const MAX_TIMEOUT_MS: u64 = 600_000;

/// Runs a command line through the host's sandbox.
#[derive(Clone, Copy, Debug)]
pub struct Shell;

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "shell",
            "Run a shell command in the workspace and return its output. \
             Use it for builds, tests and version control; prefer `read_file` and `search` \
             for reading, which do not start a process. \
             A command that changes anything outside the workspace will be refused.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The command line, run with `sh -c`."
                    },
                    "cwd": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Working directory, relative to the workspace root."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_TIMEOUT_MS,
                        "description": "Wall-clock budget. The run's own budget still applies."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        )
        .expect("the shell spec is a literal")
        .with_annotations(ToolAnnotations {
            read_only: false,
            idempotent: false,
            // Honest. It does not change what the gate does — `default.destructive` matches
            // a shell call on its command, not on this flag — but a UI reads it too.
            destructive: true,
            network: true,
            expected_duration_ms: None,
        })
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        let command = input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| rivet_core::Error::invalid_argument("`command` is required"))?;

        // Bound to `-c`, in one call. The shell reads what follows `-c` as the command
        // whatever it starts with, and there is no door in `Argv` that would let this land
        // free-standing instead.
        let mut argv = Argv::new();
        argv.option(Consuming::COMMAND, command);
        let mut spec = argv.into_exec("sh");
        spec.cwd = input
            .get("cwd")
            .and_then(serde_json::Value::as_str)
            .map(std::path::PathBuf::from);
        // The narrower of the two. A tool that could widen the context's budget would be a
        // tool that could opt out of the run's deadline.
        spec.timeout_ms = narrower(
            input.get("timeout_ms").and_then(serde_json::Value::as_u64),
            ctx.data.timeout_ms,
        );
        spec.max_output_bytes = ctx.data.max_output_bytes;

        let output = ctx.host.exec(spec).await?;
        Ok(render(command, &output))
    }
}

/// The smaller of two budgets, treating "no opinion" as no limit.
fn narrower(asked: Option<u64>, budget: Option<u64>) -> Option<u64> {
    match (asked, budget) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Assemble what the model reads, and what a UI can inspect.
fn render(command: &str, output: &rivet_core::sandbox::ExecOutput) -> ToolResult {
    let mut content = String::new();
    if output.timed_out {
        content.push_str("the command was stopped when its time ran out\n");
    }
    match output.exit_code {
        Some(code) => {
            let _ = writeln!(content, "exit code {code}");
        }
        None if !output.timed_out => content.push_str("the command was stopped\n"),
        None => {}
    }
    if !output.stdout.is_empty() {
        let _ = writeln!(content, "stdout:\n{}", output.stdout.trim_end());
    }
    if !output.stderr.is_empty() {
        let _ = writeln!(content, "stderr:\n{}", output.stderr.trim_end());
    }
    if output.stdout.is_empty() && output.stderr.is_empty() {
        content.push_str("(no output)\n");
    }

    // A non-zero exit is a failure the *model* should react to — a failing test, a compile
    // error — not a runtime failure. Returning `Err` here would tell the loop the runtime
    // broke, and the model would never see the compiler's message.
    let mut result = if output.succeeded() {
        ToolResult::ok(content)
    } else {
        ToolResult::error(content)
    };

    // `ExecOutput.stdout` is a `String`, so bytes that were not UTF-8 arrived as
    // replacement characters. Saying so beats substituting them in silence:
    // `docs/architecture.md` §11-7 asked whether to widen the contract to `Vec<u8>`, and the
    // answer is that a model cannot read those bytes either — but a UI can say the output
    // was mangled.
    let lossy = output.stdout.contains('\u{fffd}') || output.stderr.contains('\u{fffd}');
    result = result.with_structured(serde_json::json!({
        "command": command,
        "exit_code": output.exit_code,
        "timed_out": output.timed_out,
        "truncated": output.truncated,
        "lossy": lossy,
    }));
    result
}

/// Registers `shell` when the profile leaves both permissions it needs.
#[derive(Clone, Debug)]
pub struct ShellPlugin {
    manifest: PluginManifest,
}

impl ShellPlugin {
    #[must_use]
    pub fn new(manifest: PluginManifest) -> Self {
        Self { manifest }
    }

    /// The tools this plugin registers under `permissions` — one, or none.
    ///
    /// Both permissions, not either: see the crate documentation on who gets a shell.
    #[must_use]
    pub fn tools_for(permissions: &PermissionSet) -> Vec<Arc<dyn Tool>> {
        if permissions.contains(&Permission::ProcessSpawn)
            && permissions.allows(&Permission::FsWrite(FsScope::Workspace))
        {
            vec![Arc::new(Shell)]
        } else {
            Vec::new()
        }
    }
}

#[async_trait]
impl Plugin for ShellPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    async fn load(&self, ctx: PluginContext) -> rivet_core::Result<PluginHandle> {
        let mut registered = Vec::new();
        for tool in Self::tools_for(&ctx.permissions) {
            let name = tool.spec().name;
            ctx.registry.register_tool(tool).await?;
            registered.push(format!("tool:{name}"));
        }
        Ok(PluginHandle::new(registered))
    }

    async fn unload(&self, _ctx: PluginContext) -> rivet_core::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::id::PluginId;

    fn manifest() -> PluginManifest {
        rivet_plugin::parse(MANIFEST_TOML).expect("the shipped manifest parses")
    }

    fn grant(permissions: &[Permission]) -> PermissionSet {
        PermissionSet::new(permissions.to_vec())
    }

    #[test]
    fn the_manifest_matches_the_crate() {
        let manifest = manifest();
        assert_eq!(manifest.id.as_str(), PLUGIN_ID);
        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert!(manifest.is_compatible_with(rivet_core::ABI_VERSION));
        assert!(PluginId::new(PLUGIN_ID).is_ok());
    }

    #[test]
    fn the_manifest_declares_every_slot_this_plugin_registers() {
        assert_eq!(
            manifest().capabilities,
            [rivet_core::capability::CapabilityKind::Tool]
        );
    }

    #[test]
    fn the_manifest_asks_for_write_access_because_a_shell_has_it() {
        // Claiming otherwise would let a profile believe it had taken write access away
        // while handing out a shell.
        assert!(
            manifest()
                .permissions
                .contains(&Permission::FsWrite(FsScope::Workspace))
        );
    }

    #[test]
    fn the_shell_is_not_registered_without_write_permission() {
        // `readonly`, `reviewer` and `production` all land here.
        let read_and_spawn = grant(&[
            Permission::FsRead(FsScope::Workspace),
            Permission::ProcessSpawn,
        ]);
        assert!(ShellPlugin::tools_for(&read_and_spawn).is_empty());
    }

    #[test]
    fn the_shell_is_not_registered_without_process_permission() {
        let write_only = grant(&[Permission::FsWrite(FsScope::Workspace)]);
        assert!(ShellPlugin::tools_for(&write_only).is_empty());
    }

    #[test]
    fn both_permissions_together_register_the_shell() {
        let developer = grant(&[
            Permission::FsRead(FsScope::Workspace),
            Permission::FsWrite(FsScope::Workspace),
            Permission::ProcessSpawn,
        ]);
        let names: Vec<String> = ShellPlugin::tools_for(&developer)
            .iter()
            .map(|t| t.spec().name)
            .collect();
        assert_eq!(names, ["shell"]);
    }

    #[test]
    fn every_spec_passes_the_runtimes_schema_check() {
        // The validator's vocabulary is closed: a tool that declares a keyword nothing
        // enforces must fail to register rather than advertise a constraint that does
        // nothing.
        rivet_runtime::schema::validate_spec(&Shell.spec()).expect("shell");
    }

    #[test]
    fn the_shell_does_not_claim_to_be_read_only() {
        // `default.grant` reads this, and a shell that claimed otherwise would run under
        // `readonly` if something ever registered it there.
        assert!(!Shell.spec().annotations.read_only);
    }

    #[test]
    fn a_narrower_timeout_wins_over_the_context_budget() {
        assert_eq!(narrower(Some(5_000), Some(30_000)), Some(5_000));
        assert_eq!(
            narrower(Some(300_000), Some(30_000)),
            Some(30_000),
            "a tool must not be able to widen the run's budget"
        );
        assert_eq!(narrower(None, Some(30_000)), Some(30_000));
        assert_eq!(narrower(Some(1), None), Some(1));
        assert_eq!(narrower(None, None), None);
    }

    #[test]
    fn a_non_zero_exit_is_the_models_problem_not_the_runtimes() {
        let output = rivet_core::sandbox::ExecOutput {
            exit_code: Some(101),
            stdout: "test failures: 3".into(),
            stderr: String::new(),
            timed_out: false,
            truncated: false,
            duration_ms: 12,
        };
        let result = render("cargo test", &output);
        assert!(result.is_error, "the model has to see it and react");
        assert!(result.content.contains("exit code 101"), "{result:?}");
        assert!(result.content.contains("test failures: 3"), "{result:?}");
    }

    #[test]
    fn a_non_utf8_stdout_is_reported_as_lossy() {
        let output = rivet_core::sandbox::ExecOutput {
            exit_code: Some(0),
            stdout: "before \u{fffd} after".into(),
            stderr: String::new(),
            timed_out: false,
            truncated: false,
            duration_ms: 1,
        };
        let structured = render("cat blob", &output)
            .structured
            .expect("structured detail");
        assert_eq!(structured["lossy"], serde_json::json!(true));
    }

    #[test]
    fn ordinary_output_is_not_reported_as_lossy() {
        let output = rivet_core::sandbox::ExecOutput {
            exit_code: Some(0),
            stdout: "all good".into(),
            stderr: String::new(),
            timed_out: false,
            truncated: false,
            duration_ms: 1,
        };
        let structured = render("true", &output)
            .structured
            .expect("structured detail");
        assert_eq!(structured["lossy"], serde_json::json!(false));
    }

    #[test]
    fn a_timeout_says_so_rather_than_reporting_an_empty_success() {
        let output = rivet_core::sandbox::ExecOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            truncated: false,
            duration_ms: 30_000,
        };
        let result = render("sleep 300", &output);
        assert!(result.is_error);
        assert!(result.content.contains("time ran out"), "{result:?}");
    }
}
