//! Where a session lives on disk.
//!
//! ```text
//! <root>/<session-id>/log.jsonl
//! ```
//!
//! One directory per session, one line per durable fact. The layout is deliberately dull:
//! `kill -9` recovery (see [`crate::recover`]) is only checkable because a torn write can
//! damage exactly one line of one file.

use std::path::{Path, PathBuf};

use rivet_core::id::SessionId;

/// Name of the log file inside a session directory.
pub const LOG_FILE: &str = "log.jsonl";

/// The directory holding one session's files.
#[must_use]
pub fn session_dir(root: &Path, id: SessionId) -> PathBuf {
    root.join(id.to_string())
}

/// The event log for one session.
#[must_use]
pub fn log_path(root: &Path, id: SessionId) -> PathBuf {
    session_dir(root, id).join(LOG_FILE)
}

/// Parse a directory name back into a [`SessionId`], ignoring anything else in `root`.
#[must_use]
pub fn session_id_from_dir(name: &str) -> Option<SessionId> {
    name.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_directory_is_named_by_its_id() {
        let id = SessionId::new();
        let path = log_path(Path::new("/state"), id);
        assert!(path.ends_with(LOG_FILE));
        let dir = path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(session_id_from_dir(dir), Some(id));
    }

    #[test]
    fn unrelated_directory_names_are_ignored() {
        assert!(session_id_from_dir("not-a-session").is_none());
        assert!(session_id_from_dir(".DS_Store").is_none());
    }

    #[test]
    fn directory_order_is_creation_order() {
        // `list` sorts by name and relies on this: UUIDv7 ids sort by mint time, so a
        // reverse name sort is newest-first without an index file to fall out of sync.
        let first = SessionId::new();
        let second = SessionId::new();
        assert!(first.to_string() < second.to_string());
    }
}
