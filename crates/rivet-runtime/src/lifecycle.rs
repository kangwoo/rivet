//! The two `runtime.*` topics the host owns.
//!
//! `runtime.*` is the host's: only whoever started the runtime knows when it started, and
//! only whoever is tearing it down knows why. Nothing inside the loop, the registry or the
//! loader is in a position to say either — which is why these are free functions taking a
//! bus rather than methods on something the runtime owns.
//!
//! They live here rather than at the call site so the topic and the payload shape are
//! decided in one place. A second host — `examples/minimal-agent`, an embedder — publishes
//! the same two events by calling the same two functions, instead of assembling an
//! envelope that is *almost* the same.
//!
//! Ordering is the contract, and `rivet-cli`'s `run.rs` is what keeps it:
//! [`started`] is published **before** plugins are discovered, and [`shutting_down`]
//! **before** they are unloaded. So a stream that carries `runtime.started` and a
//! `plugin.discovered` but neither `plugin.loaded` nor `plugin.load.failed` is a plugin
//! whose `load` never returned — the one diagnostic Phase 3 gets for free from the
//! missing deadline `docs/architecture.md` §11-15 names.

use rivet_core::event::{Event, EventBus, EventEnvelope, RuntimeEvent};

/// Announce that a runtime is up, before anything it would describe happens.
pub fn started(bus: &dyn EventBus, version: &str) {
    bus.publish(EventEnvelope::new(Event::Runtime(RuntimeEvent::Started {
        version: version.to_string(),
    })));
}

/// Announce that a runtime is coming down, before its plugins are unloaded.
///
/// Before, not after: a subscriber that is about to be detached should see the reason it
/// is being detached. The `plugin.unloaded` events that follow are the ones an unloaded
/// plugin's own subscription necessarily misses.
pub fn shutting_down(bus: &dyn EventBus, reason: &str) {
    bus.publish(EventEnvelope::new(Event::Runtime(
        RuntimeEvent::ShuttingDown {
            reason: reason.to_string(),
        },
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::BroadcastBus;

    #[tokio::test]
    async fn the_two_topics_are_the_ones_the_host_owns() {
        let bus = BroadcastBus::new();
        let mut rx = bus.subscribe_raw();
        started(&bus, "0.1.0");
        shutting_down(&bus, "run finished");

        assert_eq!(rx.recv().await.unwrap().topic(), "runtime.started");
        assert_eq!(rx.recv().await.unwrap().topic(), "runtime.shutting_down");
    }
}
