//! # rivet-runtime
//!
//! The default implementation of the `rivet-core` contracts. Everything here is
//! replaceable: a host can build its own loop against the same traits.
//!
//! Module boundaries mirror the pipeline in `docs/architecture.md`:
//!
//! ```text
//! registry   -> what capabilities exist
//! bus        -> observation and fan-out
//! lifecycle  -> the two `runtime.*` topics the host owns  (Phase 3)
//! context    -> assemble the request        (Phase 1)
//! agent_loop -> drive model turns           (Phase 1)
//! dispatch   -> policy -> sandbox -> exec   (Phase 1 + Phase 4)
//! fsguard    -> post-open path containment  (Phase 1)
//! ```

pub mod agent_loop;
pub mod bus;
pub mod context;
pub mod digest;
pub mod dispatch;
pub mod fsguard;
pub mod jitter;
pub mod lifecycle;
pub mod registry;
pub mod schema;
pub mod session_log;
pub mod session_recovery;
pub mod workspace;

pub use bus::{BroadcastBus, Drained, Observer};
pub use context::ContextAssembler;
pub use registry::Registry;
