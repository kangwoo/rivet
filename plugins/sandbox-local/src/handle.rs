//! One prepared environment, and the process it runs.
//!
//! # Output is capped but never left unread
//!
//! Past `max_output_bytes` the reader keeps reading and throws the rest away. Stopping
//! instead would fill the pipe, block the child in `write`, and turn "produced too much
//! output" into "hung until the timeout" — a failure that looks like something else
//! entirely.
//!
//! # The readers are never waited on to the end
//!
//! A pipe closes when the *last* writer lets go, and a grandchild inherits its parent's
//! stdout. So `sh -c "sleep 300 & exit 0"` leaves a process holding the pipe after the
//! child is gone, and awaiting the reader would hang the dispatcher on a call that has
//! already finished. Instead each reader appends into a shared buffer, and after the child
//! is reaped they get [`OUTPUT_SETTLE`] to finish before being dropped. What that can cost
//! is the tail of the output, which is a cost `ExecOutput::truncated` already exists to
//! report.
//!
//! # A timeout kills the group, not the child
//!
//! See [`crate::group`]. The child leads its own process group precisely so that this can
//! signal the group, and `teardown` signals whatever is still running.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use rivet_core::id::SandboxId;
use rivet_core::sandbox::{ExecOutput, ExecSpec, SandboxHandle};
use rivet_core::workspace::Workspace;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::group;

/// Bytes kept when a caller names no cap. Matches the dispatcher's own default.
const DEFAULT_MAX_OUTPUT_BYTES: u64 = 65_536;

/// How long the output readers get after the child is gone.
///
/// A number invented here, and allowed to be: what exceeding it costs is already written
/// into the contract — the output is marked `truncated`. (The rule is the one
/// `docs/architecture.md` §11-15 turns on: invent a budget when the price of overrunning it
/// is already specified, and not otherwise.) What it buys is that a grandchild holding the
/// inherited stdout cannot wedge a call that has already ended.
pub const OUTPUT_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

/// A prepared local environment.
///
/// "Prepared" is almost nothing here — there is no container to start. What the handle owns
/// is the set of process groups it has started, so that [`SandboxHandle::teardown`] has
/// something to release.
#[derive(Debug)]
pub struct LocalHandle {
    id: SandboxId,
    workspace: Workspace,
    env_passthrough: Vec<String>,
    /// Process groups started by this handle and not yet reaped.
    live: Arc<Mutex<BTreeSet<u32>>>,
}

impl LocalHandle {
    #[must_use]
    pub fn new(workspace: Workspace, env_passthrough: Vec<String>) -> Self {
        Self {
            id: SandboxId::new(),
            workspace,
            env_passthrough,
            live: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    /// Resolve a child's working directory, refusing one that really lives outside.
    ///
    /// `fsguard::resolve_dir` canonicalizes and re-checks, so a directory symlink inside
    /// the workspace that points outside it is refused here — this is the symlink gate for
    /// a process, the counterpart of the one the file tools get.
    ///
    /// # Errors
    /// [`rivet_core::error::ErrorKind::PolicyDenied`] for an escape, `NotFound` for a
    /// directory that does not exist.
    pub fn resolve_cwd(&self, cwd: Option<&Path>) -> rivet_core::Result<PathBuf> {
        rivet_runtime::fsguard::resolve_dir(&self.workspace, cwd.unwrap_or(Path::new(".")))
    }

    /// The environment a child starts with: nothing, then the listed names, then the spec's.
    ///
    /// Names, not values: the list an operator writes says *which* variables leak, and the
    /// values stay out of the config file and out of the log.
    fn environment(&self, spec: &ExecSpec) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = self
            .env_passthrough
            .iter()
            .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
            .collect();
        // The spec wins: a caller that set a name explicitly meant that value.
        env.retain(|(name, _)| !spec.env.contains_key(name));
        env.extend(spec.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        env
    }
}

#[async_trait]
impl SandboxHandle for LocalHandle {
    fn id(&self) -> SandboxId {
        self.id
    }

    async fn exec(
        &self,
        spec: ExecSpec,
        cancel: CancellationToken,
    ) -> rivet_core::Result<ExecOutput> {
        let started = Instant::now();
        let cwd = self.resolve_cwd(spec.cwd.as_deref())?;

        let mut command = tokio::process::Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&cwd)
            .env_clear()
            .envs(self.environment(&spec))
            .stdin(if spec.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        // A safe method. The child becomes the leader of its own group, and that group is
        // the unit a cancellation kills -- otherwise `sh -c "cargo test"` leaves `rustc`.
        command.process_group(0);

        let mut child = command.spawn().map_err(|e| {
            rivet_core::Error::internal(format!("could not start `{}`", spec.program)).with_cause(e)
        })?;
        let pgid = child.id();
        if let Some(pgid) = pgid {
            self.live.lock().await.insert(pgid);
        }

        if let (Some(text), Some(mut stdin)) = (spec.stdin.as_ref(), child.stdin.take()) {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(text.as_bytes()).await;
            drop(stdin);
        }

        let cap = spec.max_output_bytes.unwrap_or(DEFAULT_MAX_OUTPUT_BYTES);
        let (stdout_buffer, out) = start_reader(child.stdout.take(), cap);
        let (stderr_buffer, err) = start_reader(child.stderr.take(), cap);

        let timeout = spec.timeout_ms.map(std::time::Duration::from_millis);
        let stop = tokio::select! {
            status = child.wait() => Stop::Exited(status),
            () = sleep_maybe(timeout) => Stop::TimedOut,
            () = cancel.cancelled() => Stop::Cancelled,
        };

        let (status, timed_out) = match stop {
            Stop::Exited(status) => (status.ok().and_then(|s| s.code()), false),
            Stop::TimedOut | Stop::Cancelled => {
                if let Some(pgid) = pgid
                    && !group::terminate(pgid).await
                {
                    // No process groups on this platform, or the group had already gone.
                    let _ = child.start_kill();
                }
                // Reap, so the child does not linger as a zombie for the life of the host.
                let _ = child.wait().await;
                (None, matches!(stop, Stop::TimedOut))
            }
        };
        if let Some(pgid) = pgid {
            self.live.lock().await.remove(&pgid);
        }

        // Bounded, not awaited to the end: an inherited pipe can outlive the child.
        let settled = settle(out, err).await;
        let (stdout, stdout_cut) = stdout_buffer.lock().await.take();
        let (stderr, stderr_cut) = stderr_buffer.lock().await.take();

        Ok(ExecOutput {
            exit_code: status,
            stdout,
            stderr,
            timed_out,
            truncated: stdout_cut || stderr_cut || !settled,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }

    /// Kill whatever this handle still has running. Idempotent.
    async fn teardown(&self) -> rivet_core::Result<()> {
        let groups: Vec<u32> = self.live.lock().await.iter().copied().collect();
        for pgid in groups {
            group::terminate(pgid).await;
            self.live.lock().await.remove(&pgid);
        }
        Ok(())
    }
}

/// Why the wait ended.
enum Stop {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Cancelled,
}

/// What one pipe has produced so far, capped.
#[derive(Debug, Default)]
struct Captured {
    kept: Vec<u8>,
    truncated: bool,
}

impl Captured {
    /// The text so far, and whether anything was thrown away.
    ///
    /// Lossy on purpose, and not silently: a caller that needs to know finds the
    /// replacement character in the result. `ExecOutput.stdout` is a `String`, and bytes a
    /// model cannot read are not worth changing the wire format over.
    fn take(&mut self) -> (String, bool) {
        (
            String::from_utf8_lossy(&std::mem::take(&mut self.kept)).into_owned(),
            self.truncated,
        )
    }
}

/// Start reading a pipe into a shared buffer, keeping at most `cap` bytes.
///
/// Reading continues past the cap on purpose: a reader that stops fills the pipe and blocks
/// the child, which converts "too much output" into "hung".
fn start_reader<R>(
    pipe: Option<R>,
    cap: u64,
) -> (Arc<Mutex<Captured>>, Option<tokio::task::JoinHandle<()>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let buffer = Arc::new(Mutex::new(Captured::default()));
    let Some(mut pipe) = pipe else {
        return (buffer, None);
    };
    let cap = usize::try_from(cap).unwrap_or(usize::MAX);
    let sink = buffer.clone();
    let task = tokio::spawn(async move {
        let mut chunk = [0u8; 8_192];
        loop {
            let read = match pipe.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let mut captured = sink.lock().await;
            let room = cap.saturating_sub(captured.kept.len());
            let take = room.min(read);
            captured.kept.extend_from_slice(&chunk[..take]);
            captured.truncated |= take < read;
        }
    });
    (buffer, Some(task))
}

/// Give the readers [`OUTPUT_SETTLE`] to finish, then **abort** them.
///
/// Returns whether they finished on their own. `false` means a writer — a grandchild holding
/// the inherited pipe — was still attached, and the output is reported as truncated.
///
/// Aborted rather than dropped. Dropping a `JoinHandle` detaches the task, so a call that
/// ran out of patience would leave a reader alive for the life of the host, still appending
/// into a buffer nobody will read again. The memory is bounded by the cap either way, so
/// this is tidiness rather than a leak — but a task that outlives the call it belongs to is
/// the kind of thing that is only ever discovered later.
///
/// The deadline is shared across both readers rather than given to each, so two pipes cannot
/// cost twice the budget.
async fn settle(
    out: Option<tokio::task::JoinHandle<()>>,
    err: Option<tokio::task::JoinHandle<()>>,
) -> bool {
    let deadline = tokio::time::Instant::now() + OUTPUT_SETTLE;
    let mut finished = true;
    for mut reader in [out, err].into_iter().flatten() {
        // `&mut handle` rather than the handle itself: a timeout that consumed it would
        // leave nothing to abort.
        if tokio::time::timeout_at(deadline, &mut reader)
            .await
            .is_err()
        {
            reader.abort();
            finished = false;
        }
    }
    finished
}

/// A deadline that never fires when there is no deadline.
async fn sleep_maybe(duration: Option<std::time::Duration>) {
    match duration {
        Some(duration) => tokio::time::sleep(duration).await,
        None => std::future::pending().await,
    }
}
