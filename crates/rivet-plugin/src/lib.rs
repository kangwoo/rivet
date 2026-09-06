//! Plugin discovery, manifest parsing, lifecycle and rollback.
//!
//! ```text
//! discover ── parse every manifest, reject duplicate ids
//!    │
//! validate ── ABI check, then manifest ∩ profile. Nothing is constructed here, so an
//!    │        incompatible plugin never gets the chance to register anything.
//!  load ───── construct, then `Plugin::load` through a [`guard::GuardedRegistry`] that
//!    │        refuses any slot the manifest did not declare and records what was
//!    │        actually registered. A failure — including a panic — unregisters the
//!    │        instance and cancels its token.
//! active ──── the whole batch loaded, so the host committed to it.
//! ```
//!
//! # What this crate deliberately does not do
//!
//! - **No dynamic loading.** In-process plugins are linked, so the loadable set is fixed
//!   at compile time. `docs/architecture.md` §8.4 forbids a dynamic ABI until the
//!   contract has been validated by a real plugin, and out-of-process is Phase 6.
//! - **No directory scan.** Discovery enumerates a catalog of [`PluginSource`] values.
//!   Walking `~/.rivet/plugins` would produce manifests nothing can instantiate — the
//!   "enabled but not loadable" middle category `docs/plan.md` says Phase 2 removes.
//!   [`Origin`] is an enum so Phase 6 can add a variant without moving the seam.
//! - **No `rivet-core` changes.** Every contract used here already exists and is already
//!   tested: [`rivet_core::plugin::PluginManifest`],
//!   [`rivet_core::capability::CapabilityVersion::accepts`],
//!   [`rivet_core::capability::PermissionSet::intersect`].

pub mod guard;
pub mod loader;
pub mod manifest;
pub mod source;

pub use guard::GuardedRegistry;
pub use loader::{LoadReport, PluginLoader, PluginRecord};
pub use manifest::{parse, parse_from};
pub use source::{Construct, Origin, PluginSource};
