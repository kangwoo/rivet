//! Workspace containment after the open (plan item 1.6b).
//!
//! `Workspace::resolve` is lexical, so these tests are about the layer that touches the
//! filesystem: a symlink inside the workspace that points outside it, a directory link, a
//! path swapped between resolving and opening, and a link used to sidestep the deny list.
//!
//! Symlink creation needs a privilege on Windows, so the escape tests are `#[cfg(unix)]`
//! and that platform is not covered. `fsguard`'s module documentation says so too.

use std::path::Path;

use rivet_core::error::ErrorKind;
use rivet_core::workspace::Workspace;
use rivet_runtime::fsguard;

/// A workspace with the deny list `rivet.example.toml` ships.
fn workspace(root: &Path) -> Workspace {
    rivet_runtime::workspace::open(
        root,
        [
            ".env",
            ".git/config",
            ".ssh",
            "**/credentials.json",
            "**/*.pem",
        ]
        .map(String::from),
    )
    .expect("the documented deny list must compile")
}

/// A workspace laid out like a small repository, plus a secret outside it.
fn repo() -> (tempfile::TempDir, tempfile::TempDir, Workspace) {
    let inside = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("passwd"), "root:x:0:0").unwrap();
    std::fs::create_dir(inside.path().join("src")).unwrap();
    std::fs::write(inside.path().join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(inside.path().join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(inside.path().join(".env"), "TOKEN=secret").unwrap();
    let ws = workspace(inside.path());
    (inside, outside, ws)
}

#[test]
fn the_workspace_root_resolves_to_itself() {
    // The regression that made this a two-entry-point module: a single "canonicalize the
    // parent" rule sends `.` to `/`, which is outside every workspace -- so `list_dir(".")`
    // and a `search` with no path, the first calls the example prompt makes, were denied.
    let (dir, _outside, ws) = repo();
    let resolved = fsguard::resolve_dir(&ws, Path::new(".")).expect("`.` must resolve");
    assert_eq!(resolved, std::fs::canonicalize(dir.path()).unwrap());
    assert_eq!(resolved, ws.root());
}

#[test]
fn ordinary_directories_and_files_pass() {
    let (_dir, _outside, ws) = repo();
    assert!(fsguard::resolve_dir(&ws, Path::new("src")).is_ok());
    let (parent, name) = fsguard::resolve_file_parent(&ws, Path::new("src/main.rs")).unwrap();
    assert!(parent.ends_with("src"));
    assert_eq!(name, "main.rs");
}

#[test]
fn the_root_is_not_a_file() {
    let (_dir, _outside, ws) = repo();
    let err = fsguard::resolve_file_parent(&ws, Path::new(".")).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert!(err.message().contains("workspace root"), "{err}");
}

#[test]
fn a_denied_path_is_refused_before_anything_is_opened() {
    let (_dir, _outside, ws) = repo();
    let err = fsguard::resolve_file_parent(&ws, Path::new(".env")).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PolicyDenied);
}

#[test]
fn traversal_out_of_the_workspace_is_refused() {
    let (_dir, _outside, ws) = repo();
    assert_eq!(
        fsguard::resolve_dir(&ws, Path::new("../"))
            .unwrap_err()
            .kind(),
        ErrorKind::PolicyDenied
    );
    assert_eq!(
        fsguard::resolve_file_parent(&ws, Path::new("../../etc/passwd"))
            .unwrap_err()
            .kind(),
        ErrorKind::PolicyDenied
    );
}

#[tokio::test]
async fn a_real_file_opens_and_reads() {
    let (_dir, _outside, ws) = repo();
    let (_file, path) = fsguard::open_read(&ws, Path::new("src/main.rs"))
        .await
        .expect("an ordinary file must open");
    assert!(path.ends_with("src/main.rs"));
}

#[tokio::test]
async fn a_missing_file_is_not_found() {
    let (_dir, _outside, ws) = repo();
    let err = fsguard::open_read(&ws, Path::new("src/nope.rs"))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[tokio::test]
async fn writing_creates_the_file_atomically() {
    let (dir, _outside, ws) = repo();
    let written = fsguard::write_atomic(&ws, Path::new("src/new.rs"), b"fn new() {}".to_vec())
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "fn new() {}");
    // No stray temporary files left behind.
    let strays: Vec<_> = std::fs::read_dir(dir.path().join("src"))
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".rivet-tmp"))
        .collect();
    assert!(strays.is_empty(), "{strays:?}");
}

#[tokio::test]
async fn writing_into_a_directory_that_does_not_exist_is_refused() {
    // Not `mkdir -p`: a write to a path whose parent is missing is nearly always a wrong
    // path, and creating directories widens the surface for no benefit.
    let (_dir, _outside, ws) = repo();
    let err = fsguard::write_atomic(&ws, Path::new("nope/here.rs"), b"x".to_vec())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[cfg(unix)]
mod symlinks {
    use super::{ErrorKind, Path, fsguard, repo};

    #[tokio::test]
    async fn a_file_link_pointing_outside_is_refused() {
        let (dir, outside, ws) = repo();
        std::os::unix::fs::symlink(outside.path().join("passwd"), dir.path().join("escape.txt"))
            .unwrap();

        let err = fsguard::open_read(&ws, Path::new("escape.txt"))
            .await
            .unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::PolicyDenied,
            "a lexical check alone would have called this file `inside`"
        );
    }

    #[tokio::test]
    async fn a_file_link_pointing_inside_is_allowed() {
        // `README.md -> docs/README.md` is a normal repository layout. Refusing it would
        // be safe and unjustified; what matters is where the link actually lands.
        let (dir, _outside, ws) = repo();
        std::os::unix::fs::symlink(
            dir.path().join("src/main.rs"),
            dir.path().join("main-link.rs"),
        )
        .unwrap();

        let (_file, resolved) = fsguard::open_read(&ws, Path::new("main-link.rs"))
            .await
            .expect("a link that stays inside must work");
        assert!(
            resolved.ends_with("src/main.rs"),
            "and it opens the real file: {}",
            resolved.display()
        );
    }

    #[test]
    fn a_directory_link_pointing_outside_is_refused() {
        // `O_NOFOLLOW` only guards the final component, so this is the case it misses.
        let (dir, outside, ws) = repo();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("elsewhere")).unwrap();

        assert_eq!(
            fsguard::resolve_dir(&ws, Path::new("elsewhere"))
                .unwrap_err()
                .kind(),
            ErrorKind::PolicyDenied
        );
        assert_eq!(
            fsguard::resolve_file_parent(&ws, Path::new("elsewhere/passwd"))
                .unwrap_err()
                .kind(),
            ErrorKind::PolicyDenied,
            "an intermediate directory link is exactly what O_NOFOLLOW does not catch"
        );
    }

    #[tokio::test]
    async fn a_link_cannot_be_used_to_sidestep_the_deny_list() {
        let (dir, _outside, ws) = repo();
        std::os::unix::fs::symlink(dir.path().join(".env"), dir.path().join("harmless.txt"))
            .unwrap();

        let err = fsguard::open_read(&ws, Path::new("harmless.txt"))
            .await
            .unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::PolicyDenied,
            "the deny list applies to where a path really goes, not to how it is spelled"
        );
    }

    #[tokio::test]
    async fn a_write_through_an_escaping_link_replaces_the_link_not_its_target() {
        // `rename` swaps the link itself, so even if a link survived resolution the write
        // would not land outside. Here it does not survive resolution either.
        let (dir, outside, ws) = repo();
        let target = outside.path().join("passwd");
        std::os::unix::fs::symlink(&target, dir.path().join("notes.txt")).unwrap();

        fsguard::write_atomic(&ws, Path::new("notes.txt"), b"overwritten".to_vec())
            .await
            .expect("writing replaces the link entry");

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "root:x:0:0",
            "the file outside the workspace must be untouched"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "overwritten"
        );
        assert!(
            !std::fs::symlink_metadata(dir.path().join("notes.txt"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself was replaced"
        );
    }

    #[tokio::test]
    async fn a_path_swapped_after_a_previous_resolution_is_refused() {
        // A resolution is not a capability: every open re-runs the whole check, so a path
        // that was legitimate a moment ago is judged again on what it is now. The
        // narrower race -- a swap between *this* resolve and *this* open -- is what the
        // descriptor's device/inode comparison in `open_read` covers; it cannot be
        // scheduled reliably from a test, so it is asserted by construction, not here.
        let (dir, outside, ws) = repo();
        let victim = dir.path().join("swap.txt");
        std::fs::write(&victim, "original").unwrap();

        let (parent, name) = fsguard::resolve_file_parent(&ws, Path::new("swap.txt")).unwrap();
        assert_eq!(
            parent.join(&name),
            std::fs::canonicalize(&victim).unwrap(),
            "it resolved cleanly while it was an ordinary file"
        );

        // Now swap the entry for a link out of the workspace.
        std::fs::remove_file(&victim).unwrap();
        std::os::unix::fs::symlink(outside.path().join("passwd"), &victim).unwrap();

        let err = fsguard::open_read(&ws, Path::new("swap.txt"))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::PolicyDenied);
    }
}
