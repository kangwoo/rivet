//! The workspace: the filesystem root an agent run is scoped to.
//!
//! Path containment is enforced here rather than in each tool, because getting it wrong
//! (symlinks, `..`, absolute paths, case-insensitive filesystems) is the single most
//! common sandbox escape in an agent.

use std::path::{Component, Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

/// A resolved workspace root plus its deny list.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
    patterns: Vec<String>,
    denied: GlobSet,
}

/// Serialized form. The compiled [`GlobSet`] is rebuilt on load, so a workspace can
/// travel to an out-of-process plugin as plain data.
#[derive(Serialize, Deserialize)]
struct WorkspaceRepr {
    root: PathBuf,
    #[serde(default)]
    deny: Vec<String>,
}

impl Serialize for Workspace {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WorkspaceRepr {
            root: self.root.clone(),
            deny: self.patterns.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Workspace {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let repr = WorkspaceRepr::deserialize(deserializer)?;
        Self::new(repr.root)
            .with_denied(repr.deny)
            .map_err(serde::de::Error::custom)
    }
}

impl Workspace {
    /// Build a workspace from an already-canonicalized root.
    ///
    /// The caller must canonicalize: `rivet-core` does no I/O, and resolving symlinks
    /// requires touching the filesystem. `rivet-runtime` provides the canonicalizing
    /// constructor.
    #[must_use]
    pub fn new(canonical_root: PathBuf) -> Self {
        Self {
            root: canonical_root,
            patterns: Vec::new(),
            denied: GlobSet::empty(),
        }
    }

    /// Attach deny patterns.
    ///
    /// Patterns are globs matched against the workspace-relative path
    /// (`.env`, `**/*.pem`, `.git/config`). A bare name like `.env` is expanded to also
    /// match at any depth, because an operator writing `.env` means "no dotenv files",
    /// not "no dotenv file in exactly the root".
    ///
    /// # Errors
    /// Returns [`crate::error::ErrorKind::InvalidArgument`] for a malformed glob, so a
    /// typo in `rivet.toml` fails at startup rather than silently protecting nothing.
    pub fn with_denied(
        mut self,
        patterns: impl IntoIterator<Item = String>,
    ) -> crate::Result<Self> {
        let patterns: Vec<String> = patterns.into_iter().collect();
        let mut builder = GlobSetBuilder::new();

        for pattern in &patterns {
            for candidate in expand(pattern) {
                let glob = Glob::new(&candidate).map_err(|e| {
                    crate::Error::invalid_argument(format!("bad deny pattern `{pattern}`"))
                        .with_cause(e)
                })?;
                builder.add(glob);
            }
        }

        self.denied = builder.build().map_err(|e| {
            crate::Error::invalid_argument("could not compile the deny list").with_cause(e)
        })?;
        self.patterns = patterns;
        Ok(self)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The deny patterns as written by the operator.
    #[must_use]
    pub fn deny_patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Resolve a candidate path against the workspace, rejecting anything that escapes.
    ///
    /// This is a *lexical* check on a normalized path. It is necessary but not
    /// sufficient: a symlink inside the workspace can still point outside it, so the
    /// runtime must re-verify after opening (`O_NOFOLLOW`, or canonicalize the opened
    /// path and re-run this check). See `docs/security.md`.
    pub fn resolve(&self, candidate: &Path) -> crate::Result<PathBuf> {
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.root.join(candidate)
        };

        let normalized = normalize_lexically(&joined);

        if !normalized.starts_with(&self.root) {
            return Err(crate::Error::policy_denied(format!(
                "path `{}` escapes the workspace root `{}`",
                candidate.display(),
                self.root.display()
            )));
        }

        let relative = normalized.strip_prefix(&self.root).unwrap_or(&normalized);
        if self.is_denied(relative) {
            return Err(crate::Error::policy_denied(format!(
                "path `{}` is on the workspace deny list",
                candidate.display()
            )));
        }

        Ok(normalized)
    }

    /// Whether a workspace-relative path is denied.
    ///
    /// Matched twice: once as written, once lowercased. macOS (APFS) and Windows are
    /// case-insensitive by default, so a deny list that only matched `.env` would let
    /// `.ENV` open the very same file.
    fn is_denied(&self, relative: &Path) -> bool {
        if self.denied.is_match(relative) {
            return true;
        }
        let lowered = relative.to_string_lossy().to_lowercase();
        self.denied.is_match(Path::new(&lowered))
    }
}

/// Expand an operator's pattern into the globs that actually implement their intent.
///
/// Two expansions, both of which a literal reading would miss:
///
/// - A pattern with no separator applies at **any depth**: `.env` covers `sub/.env`.
///   Someone writing `.env` means "no dotenv files", not "not in the root".
/// - Every pattern also denies everything **beneath** it: `.ssh` covers `.ssh/id_rsa`.
///   Denying a directory while allowing its contents protects nothing, and `.ssh` is on
///   the recommended deny list precisely for the keys inside it.
///
/// A pattern that already ends in `*` or `**` is left alone; the caller was explicit.
fn expand(pattern: &str) -> Vec<String> {
    let mut out = vec![pattern.to_string()];

    if !pattern.ends_with('*') {
        // Deny the subtree as well as the entry itself.
        out.push(format!("{pattern}/**"));
    }

    if !pattern.contains('/') {
        out.push(format!("**/{pattern}"));
        if !pattern.ends_with('*') {
            out.push(format!("**/{pattern}/**"));
        }
    }

    out
}

/// Collapse `.` and `..` without consulting the filesystem.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Popping past the prefix/root is a no-op, which keeps `/..` == `/`.
                if !matches!(
                    out.components().next_back(),
                    None | Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly the list shipped in `rivet.example.toml` and recommended by
    /// `docs/security.md`. If these stop matching, operators who follow the checklist
    /// believe they are protected while nothing is blocked.
    fn workspace() -> Workspace {
        Workspace::new(PathBuf::from("/repo"))
            .with_denied(
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

    #[test]
    fn relative_paths_resolve_under_the_root() {
        let ws = workspace();
        assert_eq!(
            ws.resolve(Path::new("src/main.rs")).unwrap(),
            PathBuf::from("/repo/src/main.rs")
        );
    }

    #[test]
    fn dot_dot_traversal_is_rejected() {
        let ws = workspace();
        assert!(ws.resolve(Path::new("../etc/passwd")).is_err());
        assert!(ws.resolve(Path::new("src/../../etc/passwd")).is_err());
    }

    #[test]
    fn interior_dot_dot_that_stays_inside_is_allowed() {
        let ws = workspace();
        assert_eq!(
            ws.resolve(Path::new("src/../Cargo.toml")).unwrap(),
            PathBuf::from("/repo/Cargo.toml")
        );
    }

    #[test]
    fn absolute_paths_outside_the_root_are_rejected() {
        let ws = workspace();
        assert!(ws.resolve(Path::new("/etc/passwd")).is_err());
        assert!(ws.resolve(Path::new("/repo/src/lib.rs")).is_ok());
    }

    #[test]
    fn sibling_prefix_is_not_treated_as_inside() {
        let ws = workspace();
        assert!(
            ws.resolve(Path::new("/repo-secrets/key")).is_err(),
            "`/repo-secrets` must not match the `/repo` prefix"
        );
    }

    #[test]
    fn the_documented_deny_list_actually_denies() {
        let ws = workspace();
        for path in [
            ".env",
            "/repo/.env",
            "sub/.env", // a bare name applies at any depth
            "deep/nested/dir/.env",
            ".git/config",
            "config/credentials.json", // ** prefix
            "credentials.json",
            "keys/server.pem", // ** with an extension glob
            "server.pem",
            ".ssh",
        ] {
            assert!(
                ws.resolve(Path::new(path)).is_err(),
                "`{path}` must be denied but was allowed"
            );
        }
    }

    #[test]
    fn denying_a_directory_denies_its_contents() {
        // Blocking `.ssh` while allowing `.ssh/id_rsa` protects nothing, and `.ssh` is on
        // the recommended list precisely because of the keys inside it.
        let ws = workspace();
        for path in [
            ".ssh",
            ".ssh/id_rsa",
            ".ssh/deep/nested/key",
            "home/.ssh/id_rsa",
            ".git/config",
        ] {
            assert!(
                ws.resolve(Path::new(path)).is_err(),
                "`{path}` must be denied but was allowed"
            );
        }
    }

    #[test]
    fn denial_is_case_insensitive() {
        // APFS and NTFS are case-insensitive by default: `.ENV` opens `.env`.
        let ws = workspace();
        assert!(ws.resolve(Path::new(".ENV")).is_err());
        assert!(ws.resolve(Path::new(".git/CONFIG")).is_err());
        assert!(ws.resolve(Path::new("keys/SERVER.PEM")).is_err());
    }

    #[test]
    fn ordinary_files_are_still_allowed() {
        let ws = workspace();
        for path in [
            "src/main.rs",
            "docs/env.md",
            "Cargo.toml",
            "envoy/config.yaml",
        ] {
            assert!(
                ws.resolve(Path::new(path)).is_ok(),
                "`{path}` must not be caught by the deny list"
            );
        }
    }

    #[test]
    fn a_malformed_pattern_fails_at_startup() {
        let err = Workspace::new(PathBuf::from("/repo"))
            .with_denied(["[".to_string()])
            .unwrap_err();
        assert!(err.message().contains("bad deny pattern"), "{err}");
    }

    #[test]
    fn a_workspace_survives_serialization() {
        let ws = workspace();
        let json = serde_json::to_string(&ws).unwrap();
        let back: Workspace = serde_json::from_str(&json).unwrap();
        assert_eq!(back.root(), ws.root());
        assert!(
            back.resolve(Path::new("sub/.env")).is_err(),
            "deny rules must survive the trip to an out-of-process plugin"
        );
    }
}
