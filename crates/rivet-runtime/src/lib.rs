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
//! context    -> assemble the request        (Phase 1)
//! agent_loop -> drive model turns           (Phase 1)
//! dispatch   -> policy -> sandbox -> exec   (Phase 4)
//! ```

pub mod agent_loop;
pub mod bus;
pub mod dispatch;
pub mod registry;

pub use bus::BroadcastBus;
pub use registry::Registry;
