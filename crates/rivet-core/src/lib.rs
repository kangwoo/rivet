//! # rivet-core
//!
//! `rivet-core` owns **contracts, not implementations**.
//!
//! Everything in this crate is one of:
//!
//! - a domain type (`Job`, `SessionEvent`, `ToolSpec`, ...)
//! - a capability trait (`Model`, `Tool`, `Policy`, `Sandbox`, ...)
//! - an event definition
//! - an error type
//! - an identifier
//! - a macro that holds one of those contracts up ([`one_of_each`])
//!
//! This crate deliberately performs **no I/O**. It does not call an LLM API, spawn a
//! process, touch the filesystem, or render a UI. Those live in `rivet-runtime` and in
//! plugin crates. If you find yourself adding `reqwest` or `std::process` here, the
//! abstraction is in the wrong place.
//!
//! ## Stability
//!
//! The traits here are the public surface every plugin compiles against. They are
//! versioned by [`ABI_VERSION`] and each capability trait carries a
//! [`CapabilityVersion`](capability::CapabilityVersion) so that out-of-process and WASM
//! plugins can negotiate compatibility without a shared Rust ABI.

pub mod agent;
pub mod capability;
pub mod context;
pub mod error;
pub mod event;
pub mod id;
pub mod job;
pub mod memory;
pub mod model;
pub mod plugin;
pub mod policy;
pub mod retry;
pub mod sandbox;
pub mod session;
pub mod time;
pub mod tool;
pub mod workspace;

pub use error::{Error, Result};
pub use time::Timestamp;

/// One value per variant of an enum, in a list that cannot fall behind the enum.
///
/// A `vec![]` literal is not exhaustiveness-checked, so on its own it cannot notice a
/// variant it is missing — and the sequence around it does not cover the gap either,
/// which is the mistake this replaces. Add a variant, a consumer's wildcard-free `match`
/// fails to compile, add the arm: everything compiles again with the variant still absent
/// from the list, so the assertion the arm just answered never runs on it. Measured on
/// the tree before this macro existed — a new `RuntimeEvent` variant, on a topic no
/// profile grants, answered "yes, the narrowed profiles are granted this" — the whole
/// suite stayed green.
///
/// Putting a wildcard-free `match` next to the list does not fix that. It moves the
/// compile error next to the list, which is not the same as forcing an entry into it: the
/// arm alone still silences it. So a pattern and its sample are **one item** here.
///
/// ```text
/// one_of_each!(Self {
///     Self::Started { .. }      => Self::Started { version: String::new() },
///     Self::ShuttingDown { .. } => Self::ShuttingDown { reason: String::new() },
/// })
/// ```
///
/// The expansion matches the samples against those patterns in a closure it never calls.
/// That `match` is exhaustiveness-checked, so a new variant fails to compile at the list;
/// and because the only syntax for an arm is `pattern => sample`, the edit that silences
/// the error is the edit that adds the sample. Nothing is left to do afterwards, so
/// nothing is left to forget. It costs nothing at run time — the closure is never called
/// and the guarantee is entirely the compiler's.
///
/// The `extend` form is the same thing one level up, for an umbrella enum whose arms each
/// contribute a whole family's samples rather than a single value.
#[macro_export]
macro_rules! one_of_each {
    (extend $enum:ty { $($pattern:pat => $samples:expr),+ $(,)? }) => {{
        let _every_variant_is_gathered = |value: &$enum| match value { $($pattern => {}),+ };
        let mut all: ::std::vec::Vec<$enum> = ::std::vec::Vec::new();
        $(all.extend($samples);)+
        all
    }};
    ($enum:ty { $($pattern:pat => $sample:expr),+ $(,)? }) => {{
        let _every_variant_has_a_sample = |value: &$enum| match value { $($pattern => {}),+ };
        ::std::vec![$($sample),+]
    }};
}

/// Version of the whole contract surface.
///
/// In-process plugins are compiled against an exact `rivet-core`, so this matters mostly
/// for out-of-process and WASM plugins, which send it during handshake. Bump the minor
/// component for additive changes, the major for breaking ones.
pub const ABI_VERSION: capability::CapabilityVersion = capability::CapabilityVersion::new(0, 1);

/// Convenience re-exports for plugin authors: `use rivet_core::prelude::*;`
pub mod prelude {
    pub use crate::agent::{AgentSpec, StopReason};
    pub use crate::context::{ContextItem, ContextProvider, ContextRequest, ContextSlot};
    pub use crate::error::{Error, Result};
    pub use crate::event::{Event, EventBus, EventEnvelope};
    pub use crate::id::{AgentId, JobId, PluginId, RunId, SessionId, ToolCallId};
    pub use crate::model::{Model, ModelId, ModelRequest, ModelStream, StreamEvent};
    pub use crate::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
    pub use crate::policy::{Policy, PolicyDecision, PolicyRequest, RestrictiveDecision};
    pub use crate::sandbox::{ExecSpec, Sandbox, SandboxHandle, SandboxRequest};
    pub use crate::session::{Expect, SessionEvent, SessionStore};
    pub use crate::tool::{
        Tool, ToolCall, ToolContext, ToolContextData, ToolHost, ToolResult, ToolSpec,
    };
    pub use async_trait::async_trait;
}
