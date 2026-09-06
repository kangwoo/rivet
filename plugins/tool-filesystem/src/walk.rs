//! Bounded, cancellation-aware directory traversal.
//!
//! Two rules make a walk safe rather than merely bounded:
//!
//! - **Links are never followed.** A symlink is reported and skipped, so a walk cannot
//!   leave the workspace no matter where a link points.
//! - **A denied entry is skipped, not raised.** These tools traverse on the model's
//!   behalf; letting one `.env` turn a whole search into `ToolBlocked` would make the
//!   deny list break searching rather than protect a secret. A path the *caller named*
//!   is different, and those denials do propagate — see [`crate::search`].

use std::path::{Path, PathBuf};

use rivet_core::tool::ToolContext;
use rivet_core::workspace::Workspace;
use rivet_runtime::fsguard::{self, EntryKind};

/// Directories that cost context and answer nothing.
pub const SKIPPED: [&str; 7] = [
    ".git",
    ".rivet",
    "target",
    "node_modules",
    ".venv",
    "dist",
    ".mypy_cache",
];

/// Limits a traversal honors so a large repository cannot wedge a turn.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_depth: usize,
    pub max_entries: usize,
}

/// One thing found while walking.
#[derive(Clone, Debug)]
pub struct Found {
    pub path: PathBuf,
    /// Path relative to the workspace root, which is what the model should see.
    pub relative: String,
    pub kind: EntryKind,
    pub depth: usize,
    pub size: Option<u64>,
}

/// What a walk produced.
#[derive(Clone, Debug, Default)]
pub struct Walk {
    pub found: Vec<Found>,
    /// Set when the entry cap stopped the walk early.
    pub truncated: bool,
    /// Set when the run was cancelled mid-walk.
    pub cancelled: bool,
}

/// Walk `root`, breadth-first by directory, honoring cancellation and the limits.
///
/// `root` must already have been through [`fsguard::resolve_dir`].
pub fn walk(ctx: &ToolContext, root: &Path, limits: Limits) -> Walk {
    let workspace = ctx.workspace();
    let mut out = Walk::default();
    visit(ctx, workspace, root, 0, limits, &mut out);
    out
}

fn visit(
    ctx: &ToolContext,
    workspace: &Workspace,
    dir: &Path,
    depth: usize,
    limits: Limits,
    out: &mut Walk,
) {
    if depth >= limits.max_depth || out.truncated || out.cancelled {
        return;
    }
    // Long walks are the natural place for a run to notice it was cancelled.
    if ctx.host.is_cancelled() {
        out.cancelled = true;
        return;
    }

    let Ok(entries) = fsguard::read_dir(workspace, dir) else {
        return;
    };
    let mut names: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .collect();
    names.sort();

    for path in names {
        if out.found.len() >= limits.max_entries {
            out.truncated = true;
            return;
        }
        if ctx.host.is_cancelled() {
            out.cancelled = true;
            return;
        }

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if SKIPPED.contains(&name.as_str()) {
            continue;
        }
        // A denied entry is skipped so the rest of the walk still works.
        if workspace.resolve(&path).is_err() {
            continue;
        }
        let Ok(kind) = fsguard::classify(&path) else {
            continue;
        };

        let relative = path
            .strip_prefix(workspace.root())
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let size = if kind == EntryKind::File {
            std::fs::metadata(&path).ok().map(|m| m.len())
        } else {
            None
        };
        out.found.push(Found {
            path: path.clone(),
            relative,
            kind,
            depth,
            size,
        });

        if kind == EntryKind::Dir {
            visit(ctx, workspace, &path, depth + 1, limits, out);
        }
        // A symlink is reported above and deliberately not descended into.
    }
}
