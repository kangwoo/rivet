//! Post-open path re-verification (plan item 1.6b).
//!
//! [`Workspace::resolve`] is a **lexical** check: it never touches the filesystem, so a
//! symlink inside the workspace pointing outside it passes. That is why this module has
//! to ship in the same commit as the first tool that opens a file — otherwise workspace
//! containment exists on paper only.
//!
//! # Two entry points, because directories and files are not the same problem
//!
//! A single "canonicalize the parent" rule rejects the workspace root itself:
//! `resolve(".")` is `Ok("/repo")`, but its parent is `/` and `resolve("/")` is denied.
//! `list_dir(".")` and a `search` with no path both land there, which is the very first
//! tool call the example prompt makes. So:
//!
//! - [`resolve_dir`] canonicalizes the **candidate itself** and re-checks containment.
//!   The root resolves to itself and passes with no special case.
//! - [`resolve_file_parent`] rejects the root (it is not a file), then hands the parent to
//!   [`resolve_dir`]. Because the lexical path is inside the root and is not the root, its
//!   parent is at worst the root itself.
//!
//! # What each layer catches
//!
//! | Layer | Catches |
//! |---|---|
//! | `Workspace::resolve` | `..`, absolute paths, deny globs — lexically |
//! | canonicalize + re-resolve | a symlink anywhere in the path pointing outside |
//! | `O_NOFOLLOW` | the final component being a link |
//! | device/inode re-check | a swap between resolving and opening |
//!
//! A link that points **inside** the workspace (`README.md -> docs/README.md`, common in
//! real repositories) is allowed: the link is resolved and the *real* path is re-checked.
//! Refusing it would be safe but unjustified.
//!
//! # Platform
//!
//! On Windows there is no `O_NOFOLLOW` equivalent here, so the guarantee is the
//! canonicalize-and-re-check pair without the descriptor-level confirmation: the
//! time-of-check/time-of-use window is wider than on Unix. The symlink-escape tests are
//! `#[cfg(unix)]` because creating a symlink on Windows needs a privilege, so that
//! platform is **not covered by tests**.
//!
//! Traversal never follows a link at all, so a walk cannot leave the workspace. The
//! remaining window — between classifying an entry and using it — is inherent to
//! path-based directory reading and is why callers re-check each entry.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rivet_core::error::{Capability, Error, ErrorKind};
use rivet_core::workspace::Workspace;

/// What a directory entry is, determined without following links.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// Reported, never followed.
    Symlink,
    Other,
}

/// Resolve an existing directory, rejecting anything that really lives outside.
///
/// # Errors
/// - [`ErrorKind::PolicyDenied`] when the path, or its real location after links are
///   resolved, escapes the workspace or is on the deny list.
/// - [`ErrorKind::NotFound`] when it does not exist.
/// - [`ErrorKind::InvalidArgument`] when it exists but is not a directory.
pub fn resolve_dir(ws: &Workspace, candidate: &Path) -> rivet_core::Result<PathBuf> {
    let lexical = ws.resolve(candidate)?;
    let real = canonicalize(&lexical)?;
    // The real path, not the one we were handed: this is where a link out of the
    // workspace is caught.
    ws.resolve(&real)?;

    let metadata = std::fs::symlink_metadata(&real).map_err(|e| io_error(&real, "stat", &e))?;
    if !metadata.is_dir() {
        return Err(Error::invalid_argument(format!(
            "`{}` is not a directory",
            real.display()
        )));
    }
    Ok(real)
}

/// Resolve a file's real parent directory and its name.
///
/// The file itself need not exist — this is also the path `write_file` takes.
///
/// # Errors
/// As [`resolve_dir`], plus [`ErrorKind::InvalidArgument`] when the candidate is the
/// workspace root, which is not a file.
pub fn resolve_file_parent(
    ws: &Workspace,
    candidate: &Path,
) -> rivet_core::Result<(PathBuf, OsString)> {
    let lexical = ws.resolve(candidate)?;
    if lexical == ws.root() {
        return Err(Error::invalid_argument(format!(
            "`{}` is the workspace root, not a file",
            candidate.display()
        )));
    }
    let (parent, name) = match (lexical.parent(), lexical.file_name()) {
        (Some(parent), Some(name)) => (parent.to_path_buf(), name.to_os_string()),
        _ => {
            return Err(Error::invalid_argument(format!(
                "`{}` does not name a file",
                candidate.display()
            )));
        }
    };

    // The lexical path is inside the root and is not the root, so its parent is at worst
    // the root itself -- `resolve_dir` never walks out.
    let real_parent = resolve_dir(ws, &parent)?;
    // Deny globs apply to the real location too: `link -> .env` must not become readable
    // by another name.
    ws.resolve(&real_parent.join(&name))?;
    Ok((real_parent, name))
}

/// Open a file for reading, re-verifying after the open.
///
/// # Errors
/// As [`resolve_file_parent`], plus [`ErrorKind::PolicyDenied`] when the final component
/// is a symlink pointing outside the workspace, and [`ErrorKind::Internal`] when the file
/// was swapped between resolution and open.
pub async fn open_read(
    ws: &Workspace,
    candidate: &Path,
) -> rivet_core::Result<(tokio::fs::File, PathBuf)> {
    let (parent, name) = resolve_file_parent(ws, candidate)?;
    let target = parent.join(name);
    let ws = ws.clone();
    let (file, path) = tokio::task::spawn_blocking(move || open_checked(&ws, &target))
        .await
        .map_err(join_failed)??;
    Ok((tokio::fs::File::from_std(file), path))
}

/// Blocking half of [`open_read`]: the `O_NOFOLLOW` open and the identity re-check.
fn open_checked(ws: &Workspace, target: &Path) -> rivet_core::Result<(std::fs::File, PathBuf)> {
    match open_nofollow(target) {
        Ok(file) => {
            verify_identity(&file, target)?;
            Ok((file, target.to_path_buf()))
        }
        Err(e) if is_symlink_refusal(&e) => {
            // The final component is a link. Resolve it and judge the *real* path: a link
            // that stays inside the workspace is legitimate and common.
            let real = canonicalize(target)?;
            ws.resolve(&real)?;
            let file = open_nofollow(&real).map_err(|e| {
                if is_symlink_refusal(&e) {
                    // A canonical path has no links left, so this means it was replaced.
                    swapped(&real)
                } else {
                    io_error(&real, "open", &e)
                }
            })?;
            verify_identity(&file, &real)?;
            Ok((file, real))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::new(
            ErrorKind::NotFound,
            Capability::Tool,
            format!("`{}` does not exist", target.display()),
        )),
        Err(e) => Err(io_error(target, "open", &e)),
    }
}

/// Write a file atomically, without ever writing through a symlink.
///
/// A temporary file is created beside the target and renamed over it. `rename` replaces a
/// symlink *itself* rather than following it, so an existing link cannot redirect the
/// write outside the workspace.
///
/// The parent directory must already exist: creating it would widen the surface for no
/// benefit, and a tool asked to write `a/b/c.rs` into a repository that has no `a/b` is
/// almost always working from a wrong path.
///
/// # Errors
/// As [`resolve_file_parent`], plus [`ErrorKind::Storage`] for any write failure.
pub async fn write_atomic(
    ws: &Workspace,
    candidate: &Path,
    bytes: Vec<u8>,
) -> rivet_core::Result<PathBuf> {
    let (parent, name) = resolve_file_parent(ws, candidate)?;
    tokio::task::spawn_blocking(move || write_atomic_blocking(&parent, &name, &bytes))
        .await
        .map_err(join_failed)?
}

fn write_atomic_blocking(
    parent: &Path,
    name: &OsString,
    bytes: &[u8],
) -> rivet_core::Result<PathBuf> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let target = parent.join(name);
    let temp = parent.join(format!(
        ".rivet-tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    nofollow(&mut options);
    let file = options
        .open(&temp)
        .map_err(|e| io_error(&temp, "create", &e))?;

    let write = (|| -> std::io::Result<()> {
        (&file).write_all(bytes)?;
        (&file).flush()?;
        file.sync_data()
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&temp);
        return Err(io_error(&temp, "write", &e));
    }

    if let Err(e) = std::fs::rename(&temp, &target) {
        let _ = std::fs::remove_file(&temp);
        return Err(io_error(&target, "replace", &e));
    }
    Ok(target)
}

/// Read a directory, having pinned it to its real location first.
///
/// # Errors
/// As [`resolve_dir`].
pub fn read_dir(ws: &Workspace, candidate: &Path) -> rivet_core::Result<std::fs::ReadDir> {
    let real = resolve_dir(ws, candidate)?;
    std::fs::read_dir(&real).map_err(|e| io_error(&real, "read", &e))
}

/// Classify a path without following a link.
///
/// # Errors
/// [`ErrorKind::Storage`] when the entry cannot be inspected.
pub fn classify(path: &Path) -> rivet_core::Result<EntryKind> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| io_error(path, "stat", &e))?;
    let kind = metadata.file_type();
    Ok(if kind.is_symlink() {
        EntryKind::Symlink
    } else if kind.is_dir() {
        EntryKind::Dir
    } else if kind.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    })
}

fn canonicalize(path: &Path) -> rivet_core::Result<PathBuf> {
    std::fs::canonicalize(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::new(
                ErrorKind::NotFound,
                Capability::Tool,
                format!("`{}` does not exist", path.display()),
            )
        } else {
            io_error(path, "resolve", &e)
        }
    })
}

#[cfg(unix)]
fn nofollow(options: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    // A safe function: the workspace forbids `unsafe`, and this does not need it.
    options.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(not(unix))]
fn nofollow(_options: &mut std::fs::OpenOptions) {
    // No equivalent. Containment still rests on canonicalize-and-re-check; see the module
    // documentation for what that does and does not guarantee.
}

fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    nofollow(&mut options);
    options.open(path)
}

#[cfg(unix)]
fn is_symlink_refusal(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_refusal(_error: &std::io::Error) -> bool {
    false
}

/// Confirm the open descriptor is the same object the path named a moment ago.
///
/// `security.md` asks for checks against a file descriptor rather than a path string;
/// this is that check. On Unix it compares device and inode. Elsewhere it is a no-op and
/// the module documentation says so.
#[cfg(unix)]
fn verify_identity(file: &std::fs::File, path: &Path) -> rivet_core::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata().map_err(|e| io_error(path, "stat", &e))?;
    let named = std::fs::symlink_metadata(path).map_err(|e| io_error(path, "stat", &e))?;
    if opened.dev() != named.dev() || opened.ino() != named.ino() {
        return Err(swapped(path));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_identity(_file: &std::fs::File, _path: &Path) -> rivet_core::Result<()> {
    Ok(())
}

fn swapped(path: &Path) -> Error {
    Error::new(
        ErrorKind::PolicyDenied,
        Capability::Tool,
        format!(
            "`{}` was replaced between resolving and opening it; refusing to use it",
            path.display()
        ),
    )
}

fn io_error(path: &Path, what: &str, cause: &std::io::Error) -> Error {
    Error::new(
        ErrorKind::Storage,
        Capability::Tool,
        format!("could not {what} `{}`", path.display()),
    )
    .with_cause(cause)
}

fn join_failed(cause: tokio::task::JoinError) -> Error {
    Error::new(
        ErrorKind::Internal,
        Capability::Tool,
        "a filesystem task did not finish",
    )
    .with_cause(cause)
}
