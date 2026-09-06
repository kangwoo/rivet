//! Session persistence: an append-only event log, replayed to rebuild state.
//!
//! Phase 1 ships [`JsonlSessionStore`]: one directory per session, one JSON line per
//! durable fact, `fsync` before every `append` returns. A `SQLite` store may follow, but
//! the file store is the one whose crash behavior can be *tested* rather than trusted —
//! see [`recover`].
//!
//! ```text
//! <root>/<session-id>/log.jsonl
//! ```
//!
//! # Concurrency
//!
//! Writers are serialized in-process and a second process is *detected*, not excluded: an
//! append whose file length does not match what this store last wrote is refused with
//! [`rivet_core::error::ErrorKind::InvalidArgument`]. Real cross-process locking is a
//! later phase; a loud refusal beats a quietly interleaved log.

pub mod jsonl;
pub mod layout;
pub mod recover;
pub mod summary;

pub use jsonl::JsonlSessionStore;
