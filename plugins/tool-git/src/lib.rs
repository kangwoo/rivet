//! `git_status`, `git_diff`, `git_log`, `git_commit` — through the sandbox, never directly.
//!
//! # The tools carry a distinction the permission vocabulary cannot
//!
//! `docs/security.md` §8 promises `reviewer` "read commands only". There is no
//! `Permission::ProcessSpawn(ReadOnly)` to express that, and inventing one would widen a
//! closed vocabulary for a single case. So the split lives in the registration conditions
//! instead:
//!
//! | Tool | Registered when | Annotation |
//! |---|---|---|
//! | `git_status` | `process_spawn` | `read_only` |
//! | `git_diff` | `process_spawn` | `read_only` |
//! | `git_log` | `process_spawn` | `read_only` |
//! | `git_commit` | `process_spawn` **and** `fs_write(workspace)` | `destructive` |
//!
//! `reviewer` and `readonly` get `process_spawn` and no write, so they get the three
//! reading commands and not the writing one — which is what that row of the table meant.
//!
//! # `--no-pager`, and no `-c`
//!
//! Every invocation passes `--no-pager`. What it does *not* pass is `-c <key>=<value>`:
//! injecting git configuration is arbitrary code execution, which is the reason
//! `.git/config` is on the shipped deny list in the first place. A tool that set config on
//! the command line would be routing around that list from inside the sandbox.
//!
//! # A path argument is normalized before it reaches argv
//!
//! `path` goes through [`rivet_runtime::fsguard`], which canonicalizes and re-checks, so a
//! directory symlink pointing outside the workspace is refused here rather than handed to
//! `git` as a relative path that happens to escape.

pub mod tools;

use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
use rivet_core::tool::Tool;

pub use tools::{GitCommit, GitDiff, GitLog, GitStatus};

/// The plugin id this crate registers under.
pub const PLUGIN_ID: &str = "rivet.tool-git";

/// This crate's `rivet-plugin.toml`, for a host catalog to hand to the loader.
pub const MANIFEST_TOML: &str = include_str!("../rivet-plugin.toml");

/// Registers the git tools the effective grant allows.
#[derive(Clone, Debug)]
pub struct GitPlugin {
    manifest: PluginManifest,
}

impl GitPlugin {
    #[must_use]
    pub fn new(manifest: PluginManifest) -> Self {
        Self { manifest }
    }

    /// The tools this plugin registers under `permissions`.
    ///
    /// Nothing at all without `process_spawn`: these are four ways to run `git`, and a
    /// profile that grants no process has no use for any of them.
    #[must_use]
    pub fn tools_for(permissions: &PermissionSet) -> Vec<Arc<dyn Tool>> {
        if !permissions.contains(&Permission::ProcessSpawn) {
            return Vec::new();
        }
        let mut tools: Vec<Arc<dyn Tool>> =
            vec![Arc::new(GitStatus), Arc::new(GitDiff), Arc::new(GitLog)];
        if permissions.allows(&Permission::FsWrite(FsScope::Workspace)) {
            tools.push(Arc::new(GitCommit));
        }
        tools
    }
}

#[async_trait]
impl Plugin for GitPlugin {
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

    fn names(permissions: &[Permission]) -> Vec<String> {
        GitPlugin::tools_for(&PermissionSet::new(permissions.to_vec()))
            .iter()
            .map(|t| t.spec().name)
            .collect()
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
    fn a_reviewer_can_read_the_diff_but_not_commit() {
        // The whole of "read commands only": `reviewer` and `readonly` hold `process_spawn`
        // and no write, and that is exactly the three reading tools.
        assert_eq!(
            names(&[
                Permission::FsRead(FsScope::Workspace),
                Permission::ProcessSpawn
            ]),
            ["git_status", "git_diff", "git_log"]
        );
    }

    #[test]
    fn a_writable_profile_also_gets_the_commit() {
        assert_eq!(
            names(&[
                Permission::FsRead(FsScope::Workspace),
                Permission::FsWrite(FsScope::Workspace),
                Permission::ProcessSpawn
            ]),
            ["git_status", "git_diff", "git_log", "git_commit"]
        );
    }

    #[test]
    fn a_profile_that_cannot_spawn_gets_nothing() {
        // `production`. Four ways to run `git` are four ways to start a process.
        assert!(
            names(&[
                Permission::FsRead(FsScope::Workspace),
                Permission::FsWrite(FsScope::Workspace)
            ])
            .is_empty()
        );
    }

    #[test]
    fn every_spec_passes_the_runtimes_schema_check() {
        let all = PermissionSet::new([
            Permission::ProcessSpawn,
            Permission::FsWrite(FsScope::Workspace),
        ]);
        for tool in GitPlugin::tools_for(&all) {
            rivet_runtime::schema::validate_spec(&tool.spec())
                .unwrap_or_else(|e| panic!("{}: {e}", tool.spec().name));
        }
    }

    #[test]
    fn the_reading_tools_declare_read_only_and_the_commit_does_not() {
        // `default.grant` reads exactly this: a tool that said nothing would be treated as
        // mutating and would not run under `readonly`.
        for tool in [GitStatus.spec(), GitDiff.spec(), GitLog.spec()] {
            assert!(tool.annotations.read_only, "{}", tool.name);
            assert!(!tool.annotations.destructive, "{}", tool.name);
        }
        let commit = GitCommit.spec();
        assert!(!commit.annotations.read_only);
        assert!(commit.annotations.destructive);
    }
}
