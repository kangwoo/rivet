//! # rivet-core
//!
//! `rivet-core` owns **contracts, not implementations**.
//!
//! Everything in this crate is one of:
//!
//! - a domain type (`Task`, `SessionEvent`, `ToolSpec`, ...)
//! - a capability trait (`Model`, `Tool`, `Policy`, `Sandbox`, ...)
//! - an event definition
//! - an error type
//! - an identifier
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
pub mod memory;
pub mod model;
pub mod plugin;
pub mod policy;
pub mod retry;
pub mod sandbox;
pub mod session;
pub mod task;
pub mod time;
pub mod tool;
pub mod workspace;

pub use error::{Error, Result};
pub use time::Timestamp;

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
    pub use crate::id::{AgentId, PluginId, RunId, SessionId, TaskId, ToolCallId};
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
