//! Tool-call id minting, injected so decoding is deterministic under test.
//!
//! The provider's own id (`call_abc123`) is discarded on arrival: [`ToolCallId`] is a
//! `UUIDv7` and changing that would be a `rivet-core` contract change. Nothing is lost —
//! the provider never sees its own id again, because every message array we send is
//! rendered from our ids (see [`crate::encode`]).
//!
//! Minting goes through this trait rather than [`ToolCallId::new`] because that reads the
//! clock, and a decoder whose output changes on every run cannot be pinned to a fixture.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_core::id::ToolCallId;
use uuid::Uuid;

/// Source of fresh [`ToolCallId`]s for a decode pass.
pub trait ToolCallIdFactory: Send + Sync + fmt::Debug {
    fn next(&self) -> ToolCallId;
}

/// The production factory: time-ordered `UUIDv7`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Uuidv7Ids;

impl ToolCallIdFactory for Uuidv7Ids {
    fn next(&self) -> ToolCallId {
        ToolCallId::new()
    }
}

/// A deterministic factory: `…0001`, `…0002`, … in call order.
///
/// Exposed rather than hidden in `tests/` so fixture assertions in any crate can compare
/// a whole decoded [`rivet_core::model::Message`] instead of picking around the ids.
#[derive(Debug, Default)]
pub struct SequentialIds {
    next: AtomicU64,
}

impl SequentialIds {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ToolCallIdFactory for SequentialIds {
    fn next(&self) -> ToolCallId {
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        ToolCallId::from_uuid(Uuid::from_u128(u128::from(n)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_ids_are_stable_across_runs() {
        let a = SequentialIds::new();
        let b = SequentialIds::new();
        assert_eq!(a.next(), b.next());
        assert_eq!(a.next(), b.next());
        assert_ne!(a.next(), a.next(), "and each call still advances");
    }

    #[test]
    fn uuid_v7_ids_are_unique() {
        let f = Uuidv7Ids;
        assert_ne!(f.next(), f.next());
    }
}
