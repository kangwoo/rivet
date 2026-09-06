//! Output modes. Both are ordinary bus subscribers.
//!
//! Neither renderer can slow the loop down: the bus is lossy by design and reports what a
//! subscriber missed as [`rivet_core::event::RuntimeEvent::SubscriberLagged`]. That is
//! also why `--jsonl` is an observation stream and not a session export.

pub mod human;
pub mod jsonl;
