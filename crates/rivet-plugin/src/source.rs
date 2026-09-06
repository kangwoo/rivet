//! Where a plugin comes from, and how to instantiate it.

use std::fmt;
use std::sync::Arc;

use rivet_core::plugin::{Plugin, PluginManifest};

/// Builds a plugin from its manifest, and from nothing else.
///
/// No config, no host handles, infallible. Everything a plugin needs to decide arrives as
/// JSON in [`rivet_core::plugin::PluginContext::config`] — which is the Phase 6 constraint
/// applied one phase early, because a remote plugin gets bytes, not a `ModelId`.
pub type Construct = fn(PluginManifest) -> Arc<dyn Plugin>;

/// Where a plugin's manifest and code came from.
///
/// One variant today. Phase 6 adds `Manifest { path }` for out-of-process plugins; the
/// enum exists now so that addition does not move the seam. Deliberately not `Copy`: the
/// Phase 6 variant carries a `PathBuf`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Linked into this binary.
    Builtin { crate_name: &'static str },
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Builtin { crate_name } => write!(f, "builtin({crate_name})"),
        }
    }
}

/// One entry in a host's plugin catalog.
#[derive(Clone, Debug)]
pub struct PluginSource {
    pub origin: Origin,
    manifest_toml: &'static str,
    construct: Construct,
}

impl PluginSource {
    /// A plugin linked into this binary.
    ///
    /// `manifest_toml` is the crate's `rivet-plugin.toml`, embedded with `include_str!`,
    /// so `rivet plugin show` and the plugin's own `manifest()` read the same bytes and
    /// cannot disagree.
    #[must_use]
    pub const fn builtin(
        crate_name: &'static str,
        manifest_toml: &'static str,
        construct: Construct,
    ) -> Self {
        Self {
            origin: Origin::Builtin { crate_name },
            manifest_toml,
            construct,
        }
    }

    #[must_use]
    pub const fn manifest_toml(&self) -> &'static str {
        self.manifest_toml
    }

    /// Parse this source's manifest, naming the origin in any error.
    pub fn manifest(&self) -> rivet_core::Result<PluginManifest> {
        crate::manifest::parse_from(self.manifest_toml, &self.origin)
    }

    /// Instantiate the plugin. Called only after the ABI check has passed.
    #[must_use]
    pub fn construct(&self, manifest: PluginManifest) -> Arc<dyn Plugin> {
        (self.construct)(manifest)
    }
}
