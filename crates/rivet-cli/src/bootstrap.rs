//! Registering the built-in plugins.
//!
//! Phase 2 replaces this file with a `PluginLoader` that discovers plugins from manifests.
//! To make that a replacement rather than a rewrite, everything here goes through the real
//! [`Plugin`] trait: a scoped registry, a `PluginContext`, `load()`, and rollback with
//! `unregister_all` when a load fails halfway.
//!
//! This lives in the CLI rather than in `rivet-runtime` because the dependency arrow runs
//! `tool-filesystem -> rivet-runtime`. Putting registration in the runtime would point it
//! back at the plugins and close the cycle.

use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::capability::CapabilityKind;
use rivet_core::context::ContextProvider;
use rivet_core::error::Error;
use rivet_core::id::{PluginId, PluginInstanceId};
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
use rivet_model_openai::OpenAiPlugin;
use rivet_runtime::context::providers::{SystemPromptProvider, WorkspaceProvider};
use rivet_runtime::registry::Owner;
use rivet_runtime::{BroadcastBus, Registry};
use rivet_tool_filesystem::FilesystemPlugin;
use tokio_util::sync::CancellationToken;

use crate::config::Config;

/// The plugin id of the built-in context providers.
pub const CONTEXT_PLUGIN_ID: &str = "rivet.context-builtin";

/// A registry with the configured plugins loaded.
#[derive(Debug)]
pub struct Loaded {
    pub registry: Registry,
    pub bus: BroadcastBus,
    /// What each plugin registered, for `rivet plugin list`.
    pub registered: Vec<String>,
    /// Enabled ids a later phase will provide.
    pub deferred: Vec<String>,
    shutdown: CancellationToken,
}

impl Loaded {
    /// Signal every plugin's background work to stop.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

/// Load the configured built-in plugins into a fresh registry.
///
/// # Errors
/// Any plugin failing to load, after that plugin's partial registrations are rolled back.
/// That includes a tool whose schema uses a keyword the runtime does not enforce:
/// [`rivet_runtime::schema::validate_spec`] runs inside `register_tool`, so such a tool
/// fails its own registration and the plugin carrying it is rolled back like any other.
pub async fn load(config: &Config) -> rivet_core::Result<Loaded> {
    let bus = BroadcastBus::new();
    let registry = Registry::new(bus.clone());
    let shutdown = CancellationToken::new();
    let mut registered = Vec::new();

    for id in &config.plugins {
        let plugin = build(id, config)?;
        let instance_id = PluginInstanceId::new();
        let manifest = plugin.manifest();
        let owner = Owner {
            plugin_id: manifest.id.clone(),
            instance_id,
        };
        let ctx = PluginContext {
            instance_id,
            // Phase 2 narrows this by the manifest; the profile's grant is the operand
            // that will be on the other side of that meet.
            permissions: config.profile.permissions(),
            manifest,
            registry: Arc::new(registry.scoped(owner)),
            events: registry.events(),
            shutdown: shutdown.clone(),
            config: config.settings_for(id),
        };

        match plugin.load(ctx).await {
            Ok(handle) => registered.extend(handle.registered),
            Err(error) => {
                // A plugin that failed halfway must leave nothing behind. This is Phase
                // 2's rollback requirement, and it is free to honor now. The token goes
                // with it: nobody downstream will ever call `Loaded::shutdown` for a load
                // that returned `Err`, so any background work an earlier plugin armed
                // would outlive the failure.
                let rolled_back = registry.unregister_all(instance_id).await;
                shutdown.cancel();
                return Err(Error::plugin(format!(
                    "plugin `{id}` failed to load (rolled back {}): {error}",
                    rolled_back.len()
                )));
            }
        }
    }

    Ok(Loaded {
        registry,
        bus,
        registered,
        deferred: config.deferred_plugins.clone(),
        shutdown,
    })
}

fn build(id: &str, config: &Config) -> rivet_core::Result<Box<dyn Plugin>> {
    match id {
        "rivet.model-openai" => Ok(Box::new(OpenAiPlugin::new(config.model.clone()))),
        "rivet.tool-filesystem" => Ok(Box::new(if config.profile.writable() {
            FilesystemPlugin::new()
        } else {
            // The profile narrows the agent's tool scope by not registering the write
            // tool at all. That is pipeline step 2, not a policy -- real enforcement is
            // Phase 4 -- but a tool the model is never offered is a tool it cannot call.
            FilesystemPlugin::read_only()
        })),
        CONTEXT_PLUGIN_ID => Ok(Box::new(ContextPlugin::new(config.instructions.clone()))),
        other => Err(Error::invalid_argument(format!(
            "no built-in plugin with id `{other}`"
        ))),
    }
}

/// The two context providers the loop cannot run without.
#[derive(Clone, Debug)]
pub struct ContextPlugin {
    instructions: String,
}

impl ContextPlugin {
    #[must_use]
    pub fn new(instructions: String) -> Self {
        Self { instructions }
    }

    /// # Panics
    /// Never: [`CONTEXT_PLUGIN_ID`] is a valid plugin id and a test says so.
    #[must_use]
    pub fn manifest_for() -> PluginManifest {
        PluginManifest::new(
            PluginId::new(CONTEXT_PLUGIN_ID).expect("CONTEXT_PLUGIN_ID is a valid plugin id"),
            "Built-in context providers",
            env!("CARGO_PKG_VERSION"),
        )
        .with_capabilities([CapabilityKind::ContextProvider])
    }
}

#[async_trait]
impl Plugin for ContextPlugin {
    fn manifest(&self) -> PluginManifest {
        Self::manifest_for()
    }

    async fn load(&self, ctx: PluginContext) -> rivet_core::Result<PluginHandle> {
        let system: Arc<dyn ContextProvider> =
            Arc::new(SystemPromptProvider::new(self.instructions.clone()));
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
    fn the_context_plugin_id_is_valid_and_matches_the_config_list() {
        assert!(PluginId::new(CONTEXT_PLUGIN_ID).is_ok());
        assert!(crate::config::IMPLEMENTED_PLUGINS.contains(&CONTEXT_PLUGIN_ID));
    }

    #[test]
    fn every_implemented_id_can_actually_be_built() {
        // The list in `config` and the match in `build` are two halves of one fact, and a
        // mismatch means an id that passes validation and then fails at startup.
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(dir.path(), &crate::config::Overrides::default()).unwrap();
        for id in crate::config::IMPLEMENTED_PLUGINS {
            build(id, &config).unwrap_or_else(|e| panic!("{id}: {e}"));
        }
    }
}
