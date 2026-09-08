//! The three policies a Rivet build ships with: containment, the grant, and a gate.
//!
//! | Policy | Answers |
//! |---|---|
//! | [`workspace::WorkspacePolicy`] | does a path-shaped argument leave the workspace? |
//! | [`grant::GrantPolicy`] | may a tool that says it mutates run under this grant? |
//! | [`destructive::DestructivePolicy`] | should a person be asked about this first? |
//!
//! All three are pure functions of [`rivet_core::policy::PolicyRequest`]. None of them
//! touches the filesystem — not even the containment one, which is *lexical* on purpose so
//! that a decision can be replayed from a log. The post-open half of containment lives in
//! `rivet_runtime::fsguard` and runs inside the tools.
//!
//! # What this plugin is not
//!
//! The destructive-command list is **not a boundary**. A command written to get past it
//! gets past it — `rm${IFS}-rf` is not `rm -rf`, and no amount of string matching fixes
//! that. What it catches is a model's mistake. The boundary is which profiles are handed a
//! shell at all (`developer` and `ci`, both of which already have workspace write access),
//! plus the fact that every argv lands in the session log.

pub mod destructive;
pub mod grant;
#[cfg(test)]
mod testing;
pub mod workspace;

use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
use rivet_core::policy::Policy;

pub use destructive::{DEFAULT_DESTRUCTIVE_COMMANDS, DestructivePolicy};
pub use grant::GrantPolicy;
pub use workspace::{PATH_KEYS, WorkspacePolicy};

/// The plugin id this crate registers under.
pub const PLUGIN_ID: &str = "rivet.policy-default";

/// This crate's `rivet-plugin.toml`, for a host catalog to hand to the loader.
pub const MANIFEST_TOML: &str = include_str!("../rivet-plugin.toml");

/// Profiles whose every tool call needs a human, unless an operator says otherwise.
///
/// A list in the *configuration* rather than a profile name in this code: an operator who
/// wants `production` to stop asking, or wants `ci` to start, changes their config. The
/// coupling stays, but it becomes a coupling to a setting.
pub const DEFAULT_APPROVE_EVERYTHING_IN: [&str; 1] = ["production"];

/// Settings from `[plugins."rivet.policy-default"]`.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Profiles in which every call requires approval.
    pub require_approval_for_all_in: Vec<String>,
    /// Command fragments that put a shell call in front of a human. Replaces the built-in
    /// list wholesale; an empty list switches the gate off, and `rivet doctor` says so.
    pub destructive_commands: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            require_approval_for_all_in: DEFAULT_APPROVE_EVERYTHING_IN
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            destructive_commands: DEFAULT_DESTRUCTIVE_COMMANDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

impl Settings {
    /// Read the plugin's table, falling back to the defaults key by key.
    #[must_use]
    pub fn from_config(config: &serde_json::Value) -> Self {
        let defaults = Self::default();
        Self {
            require_approval_for_all_in: strings(config, "require_approval_for_all_in")
                .unwrap_or(defaults.require_approval_for_all_in),
            destructive_commands: strings(config, "destructive_commands")
                .unwrap_or(defaults.destructive_commands),
        }
    }
}

fn strings(config: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    Some(
        config
            .get(key)?
            .as_array()?
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
    )
}

/// Registers the three default policies.
#[derive(Clone, Debug)]
pub struct DefaultPolicyPlugin {
    manifest: PluginManifest,
}

impl DefaultPolicyPlugin {
    #[must_use]
    pub fn new(manifest: PluginManifest) -> Self {
        Self { manifest }
    }

    /// The policies this plugin registers under `settings`.
    ///
    /// No permission gates any of them: a policy reads nothing but the request it is
    /// handed, so a narrowed profile has nothing to take away. What a profile changes is
    /// what the policies *decide*, which is the point.
    #[must_use]
    pub fn policies(settings: &Settings) -> Vec<Arc<dyn Policy>> {
        vec![
            Arc::new(DestructivePolicy::new(settings.clone())),
            Arc::new(GrantPolicy),
            Arc::new(WorkspacePolicy),
        ]
    }
}

#[async_trait]
impl Plugin for DefaultPolicyPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    async fn load(&self, ctx: PluginContext) -> rivet_core::Result<PluginHandle> {
        let settings = Settings::from_config(&ctx.config);
        let mut registered = Vec::new();
        for policy in Self::policies(&settings) {
            let name = policy.name().to_string();
            ctx.registry.register_policy(policy).await?;
            registered.push(format!("policy:{name}"));
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
            [rivet_core::capability::CapabilityKind::Policy]
        );
    }

    #[test]
    fn a_policy_asks_for_no_permission_at_all() {
        // The condition that makes an audit reproducible from the log: a policy that could
        // read a file could decide on something the log does not contain.
        assert!(manifest().permissions.is_empty());
    }

    #[test]
    fn the_three_policies_register_under_stable_names() {
        let names: Vec<String> = DefaultPolicyPlugin::policies(&Settings::default())
            .iter()
            .map(|p| p.name().to_string())
            .collect();
        assert_eq!(
            names,
            ["default.destructive", "default.grant", "default.workspace"]
        );
    }

    #[test]
    fn settings_fall_back_key_by_key() {
        let partial = serde_json::json!({ "require_approval_for_all_in": ["ci"] });
        let settings = Settings::from_config(&partial);
        assert_eq!(settings.require_approval_for_all_in, ["ci"]);
        assert_eq!(
            settings.destructive_commands.len(),
            DEFAULT_DESTRUCTIVE_COMMANDS.len(),
            "an unset key keeps its default rather than emptying the list"
        );
    }

    #[test]
    fn an_empty_destructive_list_switches_the_gate_off() {
        // Not a fallback to the defaults: an operator who writes `[]` means it, and
        // `rivet doctor` is where they are told what that costs.
        let off = serde_json::json!({ "destructive_commands": [] });
        assert!(Settings::from_config(&off).destructive_commands.is_empty());
    }
}
