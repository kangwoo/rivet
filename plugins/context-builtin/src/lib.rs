//! The two context providers the agent loop cannot run without, as an ordinary plugin.
//!
//! They live behind the same manifest, the same guard and the same rollback as every
//! other plugin. Keeping them wired by hand in the host would have left exactly one
//! plugin whose manifest is not a file and whose construction is a special case — the
//! last surviving arm of the `match` Phase 2 exists to delete.

use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::context::ContextProvider;
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
use rivet_runtime::context::providers::{SystemPromptProvider, WorkspaceProvider};

/// The plugin id this crate registers under.
pub const PLUGIN_ID: &str = "rivet.context-builtin";

/// This crate's `rivet-plugin.toml`, for a host catalog to hand to the loader.
pub const MANIFEST_TOML: &str = include_str!("../rivet-plugin.toml");

/// Registers [`SystemPromptProvider`] and [`WorkspaceProvider`].
#[derive(Clone, Debug)]
pub struct ContextPlugin {
    manifest: PluginManifest,
}

impl ContextPlugin {
    #[must_use]
    pub fn new(manifest: PluginManifest) -> Self {
        Self { manifest }
    }
}

#[async_trait]
impl Plugin for ContextPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    async fn load(&self, ctx: PluginContext) -> rivet_core::Result<PluginHandle> {
        // Absent instructions are the common case, not an error: the runtime preamble
        // alone is a complete system prompt.
        let instructions = ctx.config["agent"]["instructions"].as_str().unwrap_or("");
        let system: Arc<dyn ContextProvider> = Arc::new(SystemPromptProvider::new(instructions));
        let workspace: Arc<dyn ContextProvider> = Arc::new(WorkspaceProvider::new());
        ctx.registry.register_context_provider(system).await?;
        ctx.registry.register_context_provider(workspace).await?;
        Ok(PluginHandle::new([
            "context:system".to_string(),
            "context:workspace".to_string(),
        ]))
    }

    async fn unload(&self, _ctx: PluginContext) -> rivet_core::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_matches_the_crate() {
        let manifest = rivet_plugin::parse(MANIFEST_TOML).expect("the shipped manifest parses");
        assert_eq!(manifest.id.as_str(), PLUGIN_ID);
        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert!(manifest.is_compatible_with(rivet_core::ABI_VERSION));
        assert_eq!(
            manifest.capabilities,
            [rivet_core::capability::CapabilityKind::ContextProvider]
        );
        assert!(
            manifest.permissions.is_empty(),
            "neither provider needs a grant of its own"
        );
    }
}
