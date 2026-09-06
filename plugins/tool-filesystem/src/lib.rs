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
use rivet_core::capability::{CapabilityKind, FsScope, Permission};
use rivet_core::id::PluginId;
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
use rivet_core::tool::Tool;

pub use list_dir::ListDir;
pub use read_file::ReadFile;
pub use search::Search;
pub use write_file::WriteFile;

/// The plugin id this crate registers under.
pub const PLUGIN_ID: &str = "rivet.tool-filesystem";

/// Registers the four filesystem tools.
///
/// `writable` is how a read-only profile disarms this plugin in Phase 1: the write tool is
/// simply not registered, so it never reaches the model's tool list. That narrows the
/// **agent's tool scope**, which is pipeline step 2 — it is not a policy, and Phase 4's
/// real enforcement is still to come.
#[derive(Clone, Copy, Debug)]
pub struct FilesystemPlugin {
    writable: bool,
}

impl Default for FilesystemPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl FilesystemPlugin {
    /// A plugin offering all four tools.
    #[must_use]
    pub fn new() -> Self {
        Self { writable: true }
    }

    /// A plugin offering only the three read-only tools.
    #[must_use]
    pub fn read_only() -> Self {
        Self { writable: false }
    }

    /// The manifest, also used by `rivet plugin show`.
    ///
    /// # Panics
    /// Never: [`PLUGIN_ID`] is a valid plugin id and there is a test that says so.
    #[must_use]
    pub fn manifest_for(writable: bool) -> PluginManifest {
        let mut permissions = vec![Permission::FsRead(FsScope::Workspace)];
        if writable {
            permissions.push(Permission::FsWrite(FsScope::Workspace));
        }
        PluginManifest::new(
            PluginId::new(PLUGIN_ID).expect("PLUGIN_ID is a valid plugin id"),
            "Filesystem tools",
            env!("CARGO_PKG_VERSION"),
        )
        .with_capabilities([CapabilityKind::Tool])
        .with_permissions(permissions)
    }

    /// The tools this plugin would register.
    #[must_use]
    pub fn tools(self) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> =
            vec![Arc::new(ReadFile), Arc::new(ListDir), Arc::new(Search)];
        if self.writable {
            tools.push(Arc::new(WriteFile));
        }
        tools
    }
}

#[async_trait]
impl Plugin for FilesystemPlugin {
    fn manifest(&self) -> PluginManifest {
        Self::manifest_for(self.writable)
    }

    async fn load(&self, ctx: PluginContext) -> rivet_core::Result<PluginHandle> {
        let mut registered = Vec::new();
        for tool in self.tools() {
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

    #[test]
    fn the_plugin_id_is_valid() {
        assert!(PluginId::new(PLUGIN_ID).is_ok());
    }

    #[test]
    fn a_read_only_plugin_does_not_offer_a_write_tool() {
        let names: Vec<String> = FilesystemPlugin::read_only()
            .tools()
            .iter()
            .map(|t| t.spec().name)
            .collect();
        assert_eq!(names, ["read_file", "list_dir", "search"]);
        assert!(
            !FilesystemPlugin::manifest_for(false)
                .permissions
                .contains(&Permission::FsWrite(FsScope::Workspace)),
            "and it does not ask for write permission either"
        );
    }

    #[test]
    fn every_spec_passes_the_runtimes_schema_check() {
        // The validator's vocabulary is closed; a tool declaring a keyword it does not
        // enforce must fail to load rather than advertise a constraint that does nothing.
        for tool in FilesystemPlugin::new().tools() {
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
