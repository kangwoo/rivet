//! Filesystem tools: `read_file`, `write_file`, `list_dir`, `search`.
//!
//! Every path these tools touch goes through [`rivet_runtime::fsguard`], which re-verifies
//! it *after* the open. `Workspace::resolve` alone is a lexical check, so a symlink inside
//! the workspace pointing outside it passes — which is why these tools and that module
//! ship together rather than a phase apart.
//!
//! # Denials in and out of a traversal
//!
//! A path the caller **named** and that is refused comes back as a refusal: the model
//! asked for that file and needs to know. A denied entry met while **walking** is skipped
//! silently, because one `.env` in a repository must not turn every search into a block.

pub mod list_dir;
pub mod read_file;
pub mod search;
pub mod walk;
pub mod write_file;

use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
use rivet_core::tool::Tool;

pub use list_dir::ListDir;
pub use read_file::ReadFile;
pub use search::Search;
pub use write_file::WriteFile;

/// The plugin id this crate registers under.
pub const PLUGIN_ID: &str = "rivet.tool-filesystem";

/// This crate's `rivet-plugin.toml`, for a host catalog to hand to the loader.
pub const MANIFEST_TOML: &str = include_str!("../rivet-plugin.toml");

/// Registers the filesystem tools the effective grant allows.
///
/// A `readonly` profile disarms this plugin by meeting `fs_write` away: `write_file` is
/// then never registered, so it never reaches the model's tool list. That narrows the
/// **agent's tool scope**, which is pipeline step 2.
///
/// It is not the only layer any more, and it was never sufficient on its own: it says
/// nothing about a *different* plugin registering a tool of the same name, and these tools
/// do not read `ctx.permissions()` when they run. Since Phase 4 the policy `default.grant`
/// closes that from the chain — a call whose tool declares `read_only == false` is refused
/// under a grant with no `fs_write`, whoever registered it.
#[derive(Clone, Debug)]
pub struct FilesystemPlugin {
    manifest: PluginManifest,
}

impl FilesystemPlugin {
    #[must_use]
    pub fn new(manifest: PluginManifest) -> Self {
        Self { manifest }
    }

    /// The tools this plugin registers under `permissions`.
    ///
    /// `allows` rather than `contains`: a profile that granted a narrower scope than the
    /// manifest asked for still grants write access, and the careful plugin must not be
    /// punished for it.
    #[must_use]
    pub fn tools_for(permissions: &PermissionSet) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> =
            vec![Arc::new(ReadFile), Arc::new(ListDir), Arc::new(Search)];
        if permissions.allows(&Permission::FsWrite(FsScope::Workspace)) {
            tools.push(Arc::new(WriteFile));
        }
        tools
    }
}

#[async_trait]
impl Plugin for FilesystemPlugin {
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

    fn names(permissions: &PermissionSet) -> Vec<String> {
        FilesystemPlugin::tools_for(permissions)
            .iter()
            .map(|t| t.spec().name)
            .collect()
    }

    fn manifest() -> PluginManifest {
        rivet_plugin::parse(MANIFEST_TOML).expect("the shipped manifest parses")
    }

    #[test]
    fn the_manifest_matches_the_crate() {
        let manifest = manifest();
        assert_eq!(manifest.id.as_str(), PLUGIN_ID);
        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert!(manifest.is_compatible_with(rivet_core::ABI_VERSION));
    }

    #[test]
    fn the_manifest_declares_every_slot_this_plugin_registers() {
        // The guard refuses an undeclared slot at load time; this catches the same
        // mismatch at build time, where the author can still fix the manifest.
        assert_eq!(
            manifest().capabilities,
            [rivet_core::capability::CapabilityKind::Tool]
        );
    }

    #[test]
    fn the_manifest_asks_for_exactly_the_workspace() {
        assert_eq!(
            manifest().permissions,
            [
                Permission::FsRead(FsScope::Workspace),
                Permission::FsWrite(FsScope::Workspace)
            ]
        );
    }

    #[test]
    fn a_grant_without_write_does_not_offer_a_write_tool() {
        // DoD 3, at the plugin end: `readonly` meets `fs_write` away, and the tool the
        // model is never offered is the tool it cannot call.
        let readonly = PermissionSet::new([Permission::FsRead(FsScope::Workspace)]);
        assert_eq!(names(&readonly), ["read_file", "list_dir", "search"]);
    }

    #[test]
    fn a_grant_with_write_offers_all_four() {
        let developer = PermissionSet::new([
            Permission::FsRead(FsScope::Workspace),
            Permission::FsWrite(FsScope::Workspace),
        ]);
        assert_eq!(
            names(&developer),
            ["read_file", "list_dir", "search", "write_file"]
        );
    }

    #[test]
    fn a_subtree_write_grant_does_not_offer_the_workspace_write_tool() {
        // `write_file` is fenced to the workspace, not to a subtree, so a grant narrower
        // than the tool's own reach must not hand it over: offering it would widen the
        // grant the profile computed.
        let narrow = PermissionSet::new([Permission::FsWrite(FsScope::Subtree("docs".into()))]);
        assert!(!names(&narrow).contains(&"write_file".to_string()));

        let wide = PermissionSet::new([Permission::FsWrite(FsScope::Anywhere)]);
        assert!(
            names(&wide).contains(&"write_file".to_string()),
            "a wider grant still covers it"
        );
    }

    #[test]
    fn the_plugin_id_is_valid() {
        assert!(PluginId::new(PLUGIN_ID).is_ok());
    }

    #[test]
    fn every_spec_passes_the_runtimes_schema_check() {
        // The validator's vocabulary is closed; a tool declaring a keyword it does not
        // enforce must fail to load rather than advertise a constraint that does nothing.
        let all = PermissionSet::new([
            Permission::FsRead(FsScope::Workspace),
            Permission::FsWrite(FsScope::Workspace),
        ]);
        for tool in FilesystemPlugin::tools_for(&all) {
            rivet_runtime::schema::validate_spec(&tool.spec())
                .unwrap_or_else(|e| panic!("{}: {e}", tool.spec().name));
        }
    }

    #[test]
    fn write_is_annotated_destructive_and_reads_are_not() {
        let write = WriteFile.spec();
        assert!(write.annotations.destructive);
        assert!(!write.annotations.read_only);
        for tool in [ReadFile.spec(), ListDir.spec(), Search.spec()] {
            assert!(tool.annotations.read_only, "{}", tool.name);
        }
    }
}
