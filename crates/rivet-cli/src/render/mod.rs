//! Output modes. All three are ordinary bus subscribers.
//!
//! None of them can slow the loop down: the bus is lossy by design and reports what a
//! subscriber missed as [`rivet_core::event::RuntimeEvent::SubscriberLagged`]. That is
//! also why `--jsonl` is an observation stream and not a session export.
//!
//! They attach through [`rivet_runtime::BroadcastBus::observe`] rather than `attach`,
//! which is what makes the *end* of the stream well defined: the host can say "deliver
//! everything published, then stop" instead of sleeping and hoping.

pub mod approve;
pub mod human;
pub mod jsonl;

use std::sync::Arc;
use std::time::Duration;

use rivet_core::event::EventSubscriber;
use rivet_runtime::{BroadcastBus, Drained, Observer};

use crate::render::human::HumanRenderer;
use crate::render::jsonl::JsonlRenderer;

/// How output is presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Human,
    Jsonl,
    /// The full-screen UI. Still opt-in: every end-to-end test asserts on the human
    /// renderer's stdout/stderr split, so making it the default would rewrite them all. As
    /// of Phase 4 it is the only mode that answers an approval with a keypress instead of a
    /// typed line — [`approve::PromptApprover`] is what the other two get.
    Tui,
}

/// The host's own consumers of the bus, and the way to end them.
///
/// Held as one value so `drive` has a single place to finish the stream, whichever mode is
/// running.
#[derive(Debug, Default)]
pub struct Observers {
    attached: Vec<Observer>,
}

impl Observers {
    /// Deliver everything published so far, then stop every observer.
    ///
    /// Returns `Truncated` if any of them ran out of budget, because a consumer reading
    /// the stream needs to know its tail is missing. The budget is per observer and the
    /// worst answer wins.
    pub async fn drain_within(self, budget: Duration) -> Drained {
        let mut worst = Drained::Complete;
        for observer in self.attached {
            if let truncated @ Drained::Truncated { .. } = observer.drain_within(budget).await {
                worst = truncated;
            }
        }
        worst
    }
}

/// Attach the host's renderer for `output`.
///
/// The TUI is not built here: it is also the thing the render loop draws, so the caller
/// constructs it and hands it in. Everything else about attaching is the same, which is
/// why it goes through the same function.
#[must_use]
pub fn attach(
    bus: &BroadcastBus,
    output: Output,
    tui: Option<Arc<dyn EventSubscriber>>,
) -> Observers {
    let subscriber: Arc<dyn EventSubscriber> = match output {
        Output::Human => Arc::new(HumanRenderer::new()),
        Output::Jsonl => Arc::new(JsonlRenderer::new()),
        Output::Tui => match tui {
            Some(tui) => tui,
            // `drive` builds the `Tui` before calling this; an empty observer set is the
            // honest answer rather than a silent fallback to a renderer the caller did not
            // ask for.
            None => return Observers::default(),
        },
    };
    Observers {
        attached: vec![bus.observe(subscriber)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rivet_core::event::{Event, EventBus, EventEnvelope, RuntimeEvent};

    /// Never returns from `on_event`, so it cannot be drained.
    #[derive(Debug)]
    struct Wedged;

    #[async_trait]
    impl EventSubscriber for Wedged {
        fn name(&self) -> &'static str {
            "wedged"
        }

        async fn on_event(&self, _envelope: &EventEnvelope) {
            std::future::pending::<()>().await;
        }
    }

    /// A cut-off stream has to be *reported*, not silently short.
    ///
    /// This is what replaced `sleep(20 ms); abort()`: with the sleep, a stream whose tail
    /// did not make it looked exactly like one that ended. `drive` turns this verdict into
    /// a line on stderr.
    #[tokio::test]
    async fn a_truncated_drain_is_reported_not_swallowed() {
        let bus = BroadcastBus::new();
        let observers = Observers {
            attached: vec![bus.observe(Arc::new(Wedged))],
        };
        bus.publish(EventEnvelope::new(Event::Runtime(RuntimeEvent::Started {
            version: "0.1.0".into(),
        })));

        let verdict = observers.drain_within(Duration::from_millis(50)).await;
        assert!(
            matches!(verdict, Drained::Truncated { .. }),
            "expected a truncated verdict, got {verdict:?}"
        );
    }

    #[tokio::test]
    async fn a_stream_that_finished_reports_complete() {
        let bus = BroadcastBus::new();
        let observers = attach(&bus, Output::Jsonl, None);
        assert_eq!(
            observers.drain_within(Duration::from_secs(2)).await,
            Drained::Complete
        );
    }
}
