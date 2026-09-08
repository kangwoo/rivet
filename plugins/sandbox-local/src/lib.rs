//! The local sandbox: this machine, with the three limits a runtime can actually apply.
//!
//! | What it does | What it does **not** |
//! |---|---|
//! | starts from an empty environment | isolate the filesystem |
//! | caps captured output and keeps draining | isolate the network |
//! | kills the whole process **group** on timeout or cancel | isolate the process namespace |
//!
//! [`LocalSandbox::guarantees`] reports all four flags as `false`, which is the honest
//! answer and the one `rivet doctor` shows an operator. A shell running here can read
//! `../../.env`; nothing in this crate pretends otherwise. What bounds a shell is which
//! profiles are handed one, and that is decided in the host.
//!
//! What it *does* enforce is worth being precise about:
//!
//! - **`cwd` cannot leave the workspace, even through a link.** It goes through
//!   [`rivet_runtime::fsguard::resolve_dir`], which canonicalizes and then re-checks — so a
//!   directory symlink inside the workspace pointing outside it is refused.
//! - **The environment is opt-in by name.** `env_clear`, then the names an operator listed
//!   in `env_passthrough`, then whatever the
//!   [`rivet_core::sandbox::ExecSpec`] carries. The *values* never
//!   appear in a config file or a log.
//! - **No orphans.** The child leads its own process group and the group is what gets
//!   signalled, so `sh -c "cargo test"` does not leave `rustc` behind.

pub mod group;
pub mod handle;

use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::capability::{Permission, PermissionSet};
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
use rivet_core::sandbox::{Sandbox, SandboxGuarantees, SandboxHandle, SandboxRequest};

pub use handle::LocalHandle;

/// The plugin id this crate registers under.
pub const PLUGIN_ID: &str = "rivet.sandbox-local";

/// The provider name, as written in `[sandbox] provider`.
pub const PROVIDER_NAME: &str = "local";

/// This crate's `rivet-plugin.toml`, for a host catalog to hand to the loader.
pub const MANIFEST_TOML: &str = include_str!("../rivet-plugin.toml");

/// Environment variable names copied from the host unless an operator says otherwise.
///
/// `TERM` is deliberately absent: with it, tools emit ANSI escapes, and what the model
/// reads is supposed to be text. `PATH` and `HOME` are here because without them `cargo`
/// and `git` do not run at all, which would make the empty environment a rule nobody keeps.
pub const DEFAULT_ENV_PASSTHROUGH: [&str; 5] = ["PATH", "HOME", "LANG", "LC_ALL", "TZ"];

/// Settings from `[plugins."rivet.sandbox-local"]`.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Names — never values — copied from the host environment into every child.
    pub env_passthrough: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            env_passthrough: DEFAULT_ENV_PASSTHROUGH
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

impl Settings {
    /// Read the plugin's table, falling back to the default list.
    #[must_use]
    pub fn from_config(config: &serde_json::Value) -> Self {
        let names = config
            .get("env_passthrough")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            });
        Self {
            env_passthrough: names.unwrap_or_else(|| Self::default().env_passthrough),
        }
    }
}

/// The provider registered under [`PROVIDER_NAME`].
#[derive(Clone, Debug)]
pub struct LocalSandbox {
    settings: Settings,
}

impl LocalSandbox {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self { settings }
    }
}

#[async_trait]
impl Sandbox for LocalSandbox {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    /// Four `false`s, and that is not a placeholder.
    ///
    /// The runtime surfaces this so an operator is never misled about what they are
    /// getting. A provider that overstated one flag would be worse than no provider,
    /// because a decision elsewhere would be made on it.
    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees {
            filesystem_isolation: false,
            network_isolation: false,
            process_isolation: false,
            copies_workspace: false,
        }
    }

    async fn prepare(&self, request: SandboxRequest) -> rivet_core::Result<Box<dyn SandboxHandle>> {
        // The grant this call actually runs under, after the chain narrowed it. A profile
        // that took `process_spawn` away is refused here rather than at the spawn, so the
        // error names the permission instead of an ENOENT.
        if !request.permissions.contains(&Permission::ProcessSpawn) {
            return Err(rivet_core::Error::policy_denied(
                "this call's grant does not include `process_spawn`, \
                 so no process may be started for it",
            ));
        }
        Ok(Box::new(LocalHandle::new(
            request.workspace,
            self.settings.env_passthrough.clone(),
        )))
    }
}

/// Registers the local provider when the effective grant allows a process at all.
#[derive(Clone, Debug)]
pub struct LocalSandboxPlugin {
    manifest: PluginManifest,
}

impl LocalSandboxPlugin {
    #[must_use]
    pub fn new(manifest: PluginManifest) -> Self {
        Self { manifest }
    }

    /// Whether this plugin has anything to register under `permissions`.
    ///
    /// `production` grants no `process_spawn`, so this plugin registers **nothing** there —
    /// and that is the intended state, not a stub. `rivet plugin show` says which
    /// permission the profile took away.
    #[must_use]
    pub fn provides(permissions: &PermissionSet) -> bool {
        permissions.contains(&Permission::ProcessSpawn)
    }
}

#[async_trait]
impl Plugin for LocalSandboxPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    async fn load(&self, ctx: PluginContext) -> rivet_core::Result<PluginHandle> {
        if !Self::provides(&ctx.permissions) {
            return Ok(PluginHandle::new(Vec::new()));
        }
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox::new(Settings::from_config(&ctx.config)));
        ctx.registry.register_sandbox(sandbox).await?;
        Ok(PluginHandle::new(vec![format!("sandbox:{PROVIDER_NAME}")]))
    }

    async fn unload(&self, _ctx: PluginContext) -> rivet_core::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::capability::{FsScope, PermissionSet};
    use rivet_core::id::PluginId;
    use rivet_core::workspace::Workspace;

    fn manifest() -> PluginManifest {
        rivet_plugin::parse(MANIFEST_TOML).expect("the shipped manifest parses")
    }

    fn request(permissions: PermissionSet) -> SandboxRequest {
        SandboxRequest {
            workspace: Workspace::new(std::path::PathBuf::from("/repo")),
            permissions,
            options: serde_json::Map::new(),
        }
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
            [rivet_core::capability::CapabilityKind::Sandbox]
        );
        assert_eq!(
            manifest().permissions,
            [
                Permission::ProcessSpawn,
                Permission::FsRead(FsScope::Workspace)
            ]
        );
    }

    #[test]
    fn guarantees_are_all_false() {
        // Honest, not aspirational: `rivet doctor` prints this, and a decision made on an
        // overstated flag would be worse than one made with no provider at all.
        let guarantees = LocalSandbox::new(Settings::default()).guarantees();
        assert_eq!(guarantees, SandboxGuarantees::default());
    }

    #[test]
    fn a_profile_without_process_spawn_registers_nothing() {
        assert!(!LocalSandboxPlugin::provides(&PermissionSet::new([
            Permission::FsRead(FsScope::Workspace)
        ])));
        assert!(LocalSandboxPlugin::provides(&PermissionSet::new([
            Permission::ProcessSpawn
        ])));
    }

    #[tokio::test]
    async fn prepare_refuses_without_process_spawn() {
        // The registration check is the profile's; this one is the *call's*, after the
        // policy chain has had its say about the grant this execution runs under.
        let error = LocalSandbox::new(Settings::default())
            .prepare(request(PermissionSet::empty()))
            .await
            .expect_err("a grant with no process permission cannot prepare one");
        assert_eq!(error.kind(), rivet_core::error::ErrorKind::PolicyDenied);
        assert!(error.message().contains("process_spawn"), "{error}");
    }

    #[test]
    fn the_default_passthrough_carries_no_terminal() {
        // With `TERM` set, tools emit ANSI escapes -- and what the model reads is text.
        assert!(!DEFAULT_ENV_PASSTHROUGH.contains(&"TERM"));
        assert!(DEFAULT_ENV_PASSTHROUGH.contains(&"PATH"));
    }

    #[test]
    fn an_operator_can_replace_the_passthrough_list() {
        let settings = Settings::from_config(&serde_json::json!({
            "env_passthrough": ["PATH", "AWS_PROFILE"]
        }));
        assert_eq!(settings.env_passthrough, ["PATH", "AWS_PROFILE"]);

        // An absent key keeps the default rather than emptying it.
        let empty = Settings::from_config(&serde_json::json!({}));
        assert_eq!(empty.env_passthrough.len(), DEFAULT_ENV_PASSTHROUGH.len());
    }
}
