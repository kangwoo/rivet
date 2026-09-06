//! The canonicalizing [`Workspace`] constructor `rivet-core` cannot provide.
//!
//! `rivet-core` does no I/O, and resolving a root to its real path requires touching the
//! filesystem. Everything downstream — session storage, `fsguard` containment, the deny
//! list — is measured against this root, so it has to be the *real* one: a workspace
//! rooted at a symlink would compare canonical paths against a non-canonical prefix and
//! let every path escape.

use std::path::Path;

use rivet_core::error::Error;
use rivet_core::workspace::Workspace;

/// Canonicalize `root` and attach `deny`.
///
/// # Errors
/// - [`rivet_core::error::ErrorKind::NotFound`] when the root does not exist.
/// - [`rivet_core::error::ErrorKind::InvalidArgument`] when the root is not a directory,
///   or when a deny glob is malformed — a typo in `rivet.toml` must fail at startup
///   rather than silently protect nothing.
pub fn open(root: &Path, deny: impl IntoIterator<Item = String>) -> rivet_core::Result<Workspace> {
    let canonical = std::fs::canonicalize(root).map_err(|e| {
        Error::new(
            rivet_core::error::ErrorKind::NotFound,
            rivet_core::error::Capability::Runtime,
            format!("workspace root `{}` cannot be resolved", root.display()),
        )
        .with_cause(e)
    })?;

    if !canonical.is_dir() {
        return Err(Error::invalid_argument(format!(
            "workspace root `{}` is not a directory",
            canonical.display()
        )));
    }

    Workspace::new(canonical).with_denied(deny)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_root_is_reported_as_not_found() {
        let err = open(Path::new("/definitely/not/here"), []).unwrap_err();
        assert_eq!(err.kind(), rivet_core::error::ErrorKind::NotFound);
    }

    #[test]
    fn a_file_is_not_a_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Cargo.toml");
        std::fs::write(&file, "[package]").unwrap();
        let err = open(&file, []).unwrap_err();
        assert!(err.message().contains("not a directory"), "{err}");
    }

    #[test]
    fn a_bad_deny_glob_fails_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        let err = open(dir.path(), ["[".to_string()]).unwrap_err();
        assert!(err.message().contains("bad deny pattern"), "{err}");
    }

    #[test]
    #[cfg(unix)]
    fn a_root_reached_through_a_symlink_is_canonicalized() {
        // Without this the root is a symlink path while every resolved file is canonical,
        // and `starts_with` fails for everything inside the workspace.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let ws = open(&link, []).unwrap();
        assert_eq!(ws.root(), std::fs::canonicalize(&real).unwrap());
        assert!(ws.resolve(Path::new("src/main.rs")).is_ok());
    }
}
