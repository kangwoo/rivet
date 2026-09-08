//! The four tools, and the one code path they share.
//!
//! # Every argument goes through `Argv`
//!
//! Not one `Vec<String>` and not one `push`. [`rivet_runtime::argv::Argv`] has four doors —
//! a `&'static str` flag the tool chose, a value bound to the option before it, a
//! free-standing operand that is refused when it could be read as an option, and a pathspec
//! that lands after the `--` separator the builder emits. Adding an argument means picking
//! one, and every one of them is safe.
//!
//! That is a change of shape rather than of rule. The rule — put `--` before a path so "a
//! path that looks like a revision is still a path" — was already here, applied at the call
//! site; `rev` was pushed two lines above it with no guard, and `git --no-pager diff
//! --output=../x` writes a file above the working directory and exits 0. A rule that lives
//! at the call site is a rule the next argument skips.

use std::fmt::Write as _;

use async_trait::async_trait;
use rivet_core::sandbox::{ExecOutput, ExecSpec};
use rivet_core::tool::{Tool, ToolAnnotations, ToolContext, ToolResult, ToolSpec};
use rivet_runtime::argv::Argv;

/// The program. Named by the tool, never by the model.
const PROGRAM: &str = "git";

/// Turn `git`'s output into what the model reads.
///
/// A non-zero exit is the *model's* problem, not the runtime's: "not a git repository" and
/// "nothing to commit" are things it should read and react to, so they come back as a
/// `ToolResult` with `is_error`, never as `Err`.
fn render(what: &str, output: &ExecOutput) -> ToolResult {
    let mut content = String::new();
    if output.timed_out {
        content.push_str("git was stopped when its time ran out\n");
    }
    if !output.stdout.is_empty() {
        content.push_str(output.stdout.trim_end());
        content.push('\n');
    }
    if !output.stderr.is_empty() {
        let _ = writeln!(content, "stderr:\n{}", output.stderr.trim_end());
    }
    if content.is_empty() {
        let _ = writeln!(content, "{what}: nothing to report");
    }
    let result = if output.succeeded() {
        ToolResult::ok(content)
    } else {
        ToolResult::error(content)
    };
    result.with_structured(serde_json::json!({
        "exit_code": output.exit_code,
        "timed_out": output.timed_out,
        "truncated": output.truncated,
    }))
}

/// A `git` invocation with the arguments every one of them carries.
///
/// `--no-pager` because a pager waiting for a keypress is a hang. **Not** `-c
/// core.pager=cat`: setting git configuration from the command line is arbitrary code
/// execution, and `.git/config` is on the shipped deny list for exactly that reason. There
/// is no door in [`Argv`] that would let a model-supplied string become a `-c` either.
fn git() -> Argv {
    let mut argv = Argv::new();
    argv.flag("--no-pager");
    argv
}

/// Run `git` with `argv`, under the context's budget and cap.
async fn run(ctx: &ToolContext, argv: Argv) -> rivet_core::Result<ExecOutput> {
    let mut spec: ExecSpec = argv.into_exec(PROGRAM);
    spec.timeout_ms = ctx.data.timeout_ms;
    spec.max_output_bytes = ctx.data.max_output_bytes;
    ctx.host.exec(spec).await
}

/// Resolve a `path` argument to a workspace-relative one, refusing anything that escapes.
///
/// The real path is what is checked, so a directory symlink pointing outside the workspace
/// is refused here rather than handed to `git` as a relative path that happens to leave.
/// Returned relative, because an absolute path in argv would be a second way to say the
/// same thing and the first way is the one `git` reports back.
fn resolve_path(ctx: &ToolContext, candidate: &str) -> rivet_core::Result<String> {
    let workspace = ctx.workspace();
    let path = std::path::Path::new(candidate);
    let real = match rivet_runtime::fsguard::resolve_dir(workspace, path) {
        Ok(dir) => dir,
        // A refusal is a refusal. Only "it is not a directory" and "it does not exist yet"
        // fall through to the file rule -- falling through on `PolicyDenied` would check
        // the *parent* of an escaping link and find it perfectly contained, which is the
        // containment bug this whole path exists to avoid.
        Err(error)
            if matches!(
                error.kind(),
                rivet_core::error::ErrorKind::InvalidArgument
                    | rivet_core::error::ErrorKind::NotFound
            ) =>
        {
            let (parent, name) = rivet_runtime::fsguard::resolve_file_parent(workspace, path)?;
            parent.join(name)
        }
        Err(error) => return Err(error),
    };
    Ok(real
        .strip_prefix(workspace.root())
        .unwrap_or(&real)
        .to_string_lossy()
        .into_owned())
}

fn read_only() -> ToolAnnotations {
    ToolAnnotations {
        read_only: true,
        idempotent: true,
        destructive: false,
        network: false,
        expected_duration_ms: None,
    }
}

fn optional_str<'a>(input: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(serde_json::Value::as_str)
}

fn flag(input: &serde_json::Value, key: &str) -> bool {
    input
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// `git status --short --branch`.
#[derive(Clone, Copy, Debug)]
pub struct GitStatus;

#[async_trait]
impl Tool for GitStatus {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "git_status",
            "Show which files in the workspace have been changed, staged or left untracked.",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
        .expect("a literal spec")
        .with_annotations(read_only())
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        let mut argv = git();
        argv.flag("status");
        argv.flag("--short");
        argv.flag("--branch");
        let output = run(&ctx, argv).await?;
        Ok(render("git status", &output))
    }
}

/// `git diff`, optionally staged, optionally against a revision, optionally scoped.
#[derive(Clone, Copy, Debug)]
pub struct GitDiff;

#[async_trait]
impl Tool for GitDiff {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "git_diff",
            "Show what changed. Without arguments this is the unstaged working tree; \
             `staged` shows what is about to be committed, and `rev` compares against a \
             revision.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path":   { "type": "string",  "minLength": 1,
                                "description": "Limit the diff to this path in the workspace." },
                    "staged": { "type": "boolean", "description": "Diff the index instead." },
                    "rev":    { "type": "string",  "minLength": 1,
                                "description": "Compare against this revision, such as \
                                                `HEAD~1` or a branch name. Must not begin \
                                                with `-`." }
                },
                "additionalProperties": false
            }),
        )
        .expect("a literal spec")
        .with_annotations(read_only())
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        let mut argv = git();
        argv.flag("diff");
        if flag(&input, "staged") {
            argv.flag("--staged");
        }
        if let Some(rev) = optional_str(&input, "rev") {
            // Free-standing, so `Argv` refuses it if it could be read as an option. This is
            // the argument that made `--output=../x` reachable.
            argv.operand("rev", rev)?;
        }
        if let Some(path) = optional_str(&input, "path") {
            argv.pathspec(&resolve_path(&ctx, path)?);
        }
        let output = run(&ctx, argv).await?;
        Ok(render("git diff", &output))
    }
}

/// `git log`, most recent first.
#[derive(Clone, Copy, Debug)]
pub struct GitLog;

#[async_trait]
impl Tool for GitLog {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "git_log",
            "List recent commits, most recent first.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path":  { "type": "string",  "minLength": 1,
                               "description": "Only commits touching this path." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200,
                               "description": "How many commits. Defaults to 20." }
                },
                "additionalProperties": false
            }),
        )
        .expect("a literal spec")
        .with_annotations(read_only())
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        let limit = input
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(20);
        let mut argv = git();
        argv.flag("log");
        // `--max-count <n>` rather than `--max-count=<n>`: the separate form binds the value
        // to the option, so the number never becomes an argv entry of its own even if a
        // later change lets a wider range through the schema.
        argv.option("--max-count", &limit.to_string());
        argv.flag("--oneline");
        argv.flag("--no-decorate");
        if let Some(path) = optional_str(&input, "path") {
            argv.pathspec(&resolve_path(&ctx, path)?);
        }
        let output = run(&ctx, argv).await?;
        Ok(render("git log", &output))
    }
}

/// `git commit -m <message>`, and nothing else.
///
/// The schema is closed, so there is no `--author`, no `--amend` and no way to pass a raw
/// flag. A commit tool that took arbitrary arguments would be a shell with extra steps.
#[derive(Clone, Copy, Debug)]
pub struct GitCommit;

#[async_trait]
impl Tool for GitCommit {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "git_commit",
            "Commit what is staged. Set `all` to stage every tracked file that changed \
             first. Untracked files are never added.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string",  "minLength": 1,
                                 "description": "The commit message." },
                    "all":     { "type": "boolean",
                                 "description": "Stage modified tracked files first." }
                },
                "required": ["message"],
                "additionalProperties": false
            }),
        )
        .expect("a literal spec")
        .with_annotations(ToolAnnotations {
            read_only: false,
            idempotent: false,
            destructive: true,
            network: false,
            expected_duration_ms: None,
        })
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        let message = optional_str(&input, "message")
            .ok_or_else(|| rivet_core::Error::invalid_argument("`message` is required"))?;
        let mut argv = git();
        argv.flag("commit");
        if flag(&input, "all") {
            argv.flag("--all");
        }
        // Bound to `-m`, which consumes the next entry verbatim -- so a message that opens
        // with `--amend` is a strange message rather than an amend, and refusing it would be
        // refusing something that was never dangerous.
        argv.option("-m", message);
        let output = run(&ctx, argv).await?;
        Ok(render("git commit", &output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_git_is_a_result_the_model_reads_not_a_runtime_error() {
        let output = ExecOutput {
            exit_code: Some(128),
            stdout: String::new(),
            stderr: "fatal: not a git repository".into(),
            timed_out: false,
            truncated: false,
            duration_ms: 4,
        };
        let result = render("git status", &output);
        assert!(result.is_error, "the model has to see it and react");
        assert!(
            result.content.contains("not a git repository"),
            "{result:?}"
        );
    }

    #[test]
    fn a_clean_repository_still_says_something() {
        // Empty output with a zero exit is `git status` on a clean tree. A blank tool
        // result reads as a broken tool.
        let output = ExecOutput {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            truncated: false,
            duration_ms: 4,
        };
        let result = render("git status", &output);
        assert!(!result.is_error);
        assert!(result.content.contains("nothing to report"), "{result:?}");
    }

    #[test]
    fn no_tool_can_inject_git_configuration() {
        // `-c key=value` is arbitrary code execution, which is why `.git/config` is denied.
        // Every invocation starts here, and `Argv::flag` takes `&'static str`, so there is
        // no door through which a model-supplied string could become one.
        assert_eq!(git().into_args(), ["--no-pager"]);
    }
}
