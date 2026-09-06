//! The JSONL session store.
//!
//! One line per durable fact, `fsync`ed before `append` returns. That is the whole design,
//! and it is deliberately duller than a database: `resume` after `kill -9` is only
//! *testable* because the failure modes are "the last line is half-written" and nothing
//! else. A `SQLite` store would hand the same recovery to a library and leave us with no
//! surface to check.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::error::{Error, ErrorKind};
use rivet_core::id::SessionId;
use rivet_core::session::{
    Expect, ForkPoint, SessionEvent, SessionStore, SessionSummary, StoredEvent,
};
use rivet_core::time::Timestamp;
use tokio::sync::Mutex;

use crate::{layout, recover, summary};

/// An open log, plus what we believe is on disk.
#[derive(Debug)]
struct Handle {
    /// Opened in append mode. `&File` implements `Write`, so a shared handle is enough
    /// and the file never has to move into the blocking closure.
    file: Arc<std::fs::File>,
    dir: PathBuf,
    path: PathBuf,
    last_seq: u64,
    /// Bytes *we* have written. A mismatch means somebody else appended.
    bytes: u64,
    /// Whether the directory entry itself has been `fsync`ed for this session.
    dir_synced: bool,
}

/// A [`SessionStore`] backed by one append-only JSONL file per session.
///
/// `root` is the directory that *holds* session directories; the CLI passes
/// `<workspace>/.rivet/sessions`.
#[derive(Debug)]
pub struct JsonlSessionStore {
    root: PathBuf,
    open: Mutex<HashMap<SessionId, Arc<Mutex<Handle>>>>,
}

impl JsonlSessionStore {
    /// Open a store rooted at `root`. The directory is created on first write.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            open: Mutex::new(HashMap::new()),
        }
    }

    /// The directory holding session directories.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the open handle for `id`, recovering a torn tail the first time.
    async fn handle(&self, id: SessionId) -> rivet_core::Result<Arc<Mutex<Handle>>> {
        if let Some(handle) = self.open.lock().await.get(&id) {
            return Ok(handle.clone());
        }

        let path = layout::log_path(&self.root, id);
        let dir = layout::session_dir(&self.root, id);
        let opened = tokio::task::spawn_blocking(move || open_existing(&dir, &path))
            .await
            .map_err(join_failed)??;

        let mut open = self.open.lock().await;
        // Another task may have opened it while we were blocking; one handle per session
        // is what makes `bytes` a reliable second-writer check.
        let entry = open
            .entry(id)
            .or_insert_with(|| Arc::new(Mutex::new(opened)));
        Ok(entry.clone())
    }

    /// Read a whole log without opening a handle or repairing anything.
    ///
    /// Used by `list`, which must not rewrite files just because somebody asked what
    /// sessions exist.
    fn read_log_readonly(path: &Path) -> rivet_core::Result<Vec<StoredEvent>> {
        let data = std::fs::read(path).map_err(|e| io_error(path, "read", &e))?;
        Ok(recover::scan(path, &data)?.events)
    }
}

/// Open an existing log, truncating a partial final line if a crash left one.
fn open_existing(dir: &Path, path: &Path) -> rivet_core::Result<Handle> {
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::NotFound,
            rivet_core::error::Capability::Session,
            format!("no session log at `{}`", path.display()),
        ));
    }

    let data = std::fs::read(path).map_err(|e| io_error(path, "read", &e))?;
    let scan = recover::scan(path, &data)?;

    if scan.torn_tail {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| io_error(path, "open for repair", &e))?;
        file.set_len(scan.good_bytes)
            .map_err(|e| io_error(path, "truncate", &e))?;
        file.sync_all().map_err(|e| io_error(path, "fsync", &e))?;
        // `RuntimeEvent` has no variant for this and adding one would be a contract
        // change, so the repair is reported here rather than on the bus.
        tracing::warn!(
            path = %path.display(),
            dropped_bytes = data.len() as u64 - scan.good_bytes,
            last_seq = scan.last_seq(),
            "truncated a partially written event at the end of a session log"
        );
    }

    let file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| io_error(path, "open", &e))?;

    Ok(Handle {
        file: Arc::new(file),
        dir: dir.to_path_buf(),
        path: path.to_path_buf(),
        last_seq: scan.last_seq(),
        bytes: scan.good_bytes,
        dir_synced: true,
    })
}

/// Serialize one event as a log line.
fn line_for(stored: &StoredEvent) -> rivet_core::Result<Vec<u8>> {
    // Serialize before touching the disk: a value that cannot be encoded must not leave a
    // half-written line behind.
    let mut line = serde_json::to_vec(stored)
        .map_err(|e| Error::storage("could not serialize a session event").with_cause(e))?;
    line.push(b'\n');
    Ok(line)
}

fn io_error(path: &Path, what: &str, cause: &std::io::Error) -> Error {
    Error::storage(format!("could not {what} `{}`", path.display())).with_cause(cause)
}

fn join_failed(cause: tokio::task::JoinError) -> Error {
    Error::storage("a session write task did not finish").with_cause(cause)
}

/// The outcome of one blocking append.
enum Written {
    Ok,
    /// The file is not the length we last wrote: another writer is appending.
    Conflict {
        actual: u64,
    },
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn create(&self, id: SessionId, event: SessionEvent) -> rivet_core::Result<StoredEvent> {
        let dir = layout::session_dir(&self.root, id);
        let path = layout::log_path(&self.root, id);
        let stored = StoredEvent {
            seq: 1,
            at: Timestamp::now(),
            event,
        };
        let line = line_for(&stored)?;
        let length = line.len() as u64;

        let (dir_for_task, path_for_task) = (dir.clone(), path.clone());
        let file = tokio::task::spawn_blocking(move || -> rivet_core::Result<std::fs::File> {
            std::fs::create_dir_all(&dir_for_task)
                .map_err(|e| io_error(&dir_for_task, "create", &e))?;
            let file = std::fs::OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(&path_for_task)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        // Opening an existing log is `append`'s job, not `create`'s.
                        Error::invalid_argument(format!(
                            "session `{id}` already exists at `{}`",
                            path_for_task.display()
                        ))
                    } else {
                        io_error(&path_for_task, "create", &e)
                    }
                })?;
            (&file)
                .write_all(&line)
                .map_err(|e| io_error(&path_for_task, "write", &e))?;
            file.sync_data()
                .map_err(|e| io_error(&path_for_task, "fsync", &e))?;
            // The file's own fsync does not make its *directory entry* durable.
            std::fs::File::open(&dir_for_task)
                .and_then(|d| d.sync_all())
                .map_err(|e| io_error(&dir_for_task, "fsync", &e))?;
            Ok(file)
        })
        .await
        .map_err(join_failed)??;

        self.open.lock().await.insert(
            id,
            Arc::new(Mutex::new(Handle {
                file: Arc::new(file),
                dir,
                path,
                last_seq: 1,
                bytes: length,
                // `create` already synced it, so the first `append` need not.
                dir_synced: true,
            })),
        );
        Ok(stored)
    }

    async fn append(
        &self,
        id: SessionId,
        expect: Expect,
        event: SessionEvent,
    ) -> rivet_core::Result<StoredEvent> {
        let handle = self.handle(id).await?;
        let mut guard = handle.lock().await;

        if let Expect::Seq(expected) = expect
            && guard.last_seq != expected
        {
            return Err(Error::invalid_argument(format!(
                "session `{id}` is at seq {} but the writer expected {expected}",
                guard.last_seq
            )));
        }

        let stored = StoredEvent {
            seq: guard.last_seq + 1,
            at: Timestamp::now(),
            event,
        };
        let line = line_for(&stored)?;
        let length = line.len() as u64;

        let file = guard.file.clone();
        let expected_bytes = guard.bytes;
        let dir = (!guard.dir_synced).then(|| guard.dir.clone());
        let path = guard.path.clone();

        // Note what is *not* here: this future is never a `select!` arm. A log write torn
        // in half by Ctrl-C is the one failure the whole design refuses to allow.
        let written = tokio::task::spawn_blocking(move || -> rivet_core::Result<Written> {
            let actual = file
                .metadata()
                .map_err(|e| io_error(&path, "stat", &e))?
                .len();
            if actual != expected_bytes {
                return Ok(Written::Conflict { actual });
            }
            (&*file)
                .write_all(&line)
                .map_err(|e| io_error(&path, "write", &e))?;
            (&*file).flush().map_err(|e| io_error(&path, "flush", &e))?;
            file.sync_data().map_err(|e| io_error(&path, "fsync", &e))?;
            if let Some(dir) = dir {
                std::fs::File::open(&dir)
                    .and_then(|d| d.sync_all())
                    .map_err(|e| io_error(&dir, "fsync", &e))?;
            }
            Ok(Written::Ok)
        })
        .await
        .map_err(join_failed)??;

        match written {
            Written::Ok => {
                guard.last_seq = stored.seq;
                guard.bytes += length;
                guard.dir_synced = true;
                Ok(stored)
            }
            // Detection, not mutual exclusion: cross-process locking is a Phase 4/5
            // question. A loud refusal beats a quietly interleaved log.
            Written::Conflict { actual } => Err(Error::invalid_argument(format!(
                "session `{id}` was modified by another writer \
                 (log is {actual} bytes, expected {expected_bytes}); refusing to append"
            ))),
        }
    }

    async fn read(
        &self,
        id: SessionId,
        from_seq: u64,
        limit: usize,
    ) -> rivet_core::Result<Vec<StoredEvent>> {
        // Opening the handle first is what guarantees a torn tail has been repaired
        // before anybody reads past it.
        let handle = self.handle(id).await?;
        let path = handle.lock().await.path.clone();

        tokio::task::spawn_blocking(move || -> rivet_core::Result<Vec<StoredEvent>> {
            let file = std::fs::File::open(&path).map_err(|e| io_error(&path, "open", &e))?;
            let mut out = Vec::new();
            // Streamed rather than slurped: a long session log should not have to fit in
            // memory to be read a page at a time.
            for line in BufReader::new(file).lines() {
                let line = line.map_err(|e| io_error(&path, "read", &e))?;
                if line.is_empty() {
                    continue;
                }
                let event: StoredEvent = serde_json::from_str(&line).map_err(|e| {
                    Error::storage(format!("session log `{}` is corrupt", path.display()))
                        .with_cause(e)
                })?;
                if event.seq < from_seq {
                    continue;
                }
                out.push(event);
                if out.len() >= limit {
                    break;
                }
            }
            Ok(out)
        })
        .await
        .map_err(join_failed)?
    }

    async fn last_seq(&self, id: SessionId) -> rivet_core::Result<u64> {
        let handle = self.handle(id).await?;
        let seq = handle.lock().await.last_seq;
        Ok(seq)
    }

    async fn list(&self, limit: usize) -> rivet_core::Result<Vec<SessionSummary>> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || -> rivet_core::Result<Vec<SessionSummary>> {
            let Ok(entries) = std::fs::read_dir(&root) else {
                // No sessions yet is not an error.
                return Ok(Vec::new());
            };

            let mut ids: Vec<(String, SessionId)> = entries
                .filter_map(std::result::Result::ok)
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    layout::session_id_from_dir(&name).map(|id| (name, id))
                })
                .collect();
            // UUIDv7 sorts by mint time, so a reverse name sort is newest-first. That is
            // why there is no index file to drift out of sync with the logs.
            ids.sort_by(|a, b| b.0.cmp(&a.0));

            let mut out = Vec::new();
            for (_, id) in ids {
                if out.len() >= limit {
                    break;
                }
                let path = layout::log_path(&root, id);
                match JsonlSessionStore::read_log_readonly(&path) {
                    Ok(events) if !events.is_empty() => {
                        out.push(summary::summarize(id, &events));
                    }
                    Ok(_) => {}
                    // One damaged log must not make `rivet session list` unusable.
                    Err(err) => tracing::warn!(
                        session = %id,
                        error = %err,
                        "skipping a session whose log could not be read"
                    ),
                }
            }
            Ok(out)
        })
        .await
        .map_err(join_failed)?
    }

    async fn fork(
        &self,
        source: SessionId,
        at_seq: u64,
        new_id: SessionId,
    ) -> rivet_core::Result<SessionSummary> {
        if at_seq == 0 {
            return Err(Error::invalid_argument(
                "cannot fork at seq 0; the first event is seq 1",
            ));
        }
        let inherited = self
            .read(source, 1, usize::try_from(at_seq).unwrap_or(usize::MAX))
            .await?;
        if inherited.len() as u64 != at_seq {
            return Err(Error::invalid_argument(format!(
                "session `{source}` has {} events; cannot fork at seq {at_seq}",
                inherited.len()
            )));
        }

        let mut events = inherited;
        // Rewrite the fork point onto the inherited `session.created`, so the new log
        // states where it came from and its `seq` stays dense from 1.
        match &mut events[0].event {
            SessionEvent::Created { parent, .. } => {
                *parent = Some(ForkPoint {
                    session_id: source,
                    at_seq,
                });
            }
            other => {
                return Err(Error::storage(format!(
                    "session `{source}` does not begin with `session.created` \
                     (found {other:?}); refusing to fork"
                )));
            }
        }

        let dir = layout::session_dir(&self.root, new_id);
        let path = layout::log_path(&self.root, new_id);
        let mut blob = Vec::new();
        for event in &events {
            blob.extend_from_slice(&line_for(event)?);
        }
        let length = blob.len() as u64;

        let (dir_for_task, path_for_task) = (dir.clone(), path.clone());
        let file = tokio::task::spawn_blocking(move || -> rivet_core::Result<std::fs::File> {
            std::fs::create_dir_all(&dir_for_task)
                .map_err(|e| io_error(&dir_for_task, "create", &e))?;
            let file = std::fs::OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(&path_for_task)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        Error::invalid_argument(format!("session `{new_id}` already exists"))
                    } else {
                        io_error(&path_for_task, "create", &e)
                    }
                })?;
            // A batch is allowed to be one write and one fsync; what is forbidden is
            // returning before the batch is durable.
            (&file)
                .write_all(&blob)
                .map_err(|e| io_error(&path_for_task, "write", &e))?;
            file.sync_data()
                .map_err(|e| io_error(&path_for_task, "fsync", &e))?;
            std::fs::File::open(&dir_for_task)
                .and_then(|d| d.sync_all())
                .map_err(|e| io_error(&dir_for_task, "fsync", &e))?;
            Ok(file)
        })
        .await
        .map_err(join_failed)??;

        self.open.lock().await.insert(
            new_id,
            Arc::new(Mutex::new(Handle {
                file: Arc::new(file),
                dir,
                path,
                last_seq: at_seq,
                bytes: length,
                dir_synced: true,
            })),
        );

        Ok(summary::summarize(new_id, &events))
    }
}
