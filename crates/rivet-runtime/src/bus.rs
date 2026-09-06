//! A broadcast event bus.
//!
//! The load-bearing decision: **publishing never blocks and never fails.** An agent loop
//! that can be stalled by a slow telemetry subscriber is a loop that will be stalled by a
//! slow telemetry subscriber. Subscribers that fall behind lose events and the bus says
//! so via [`rivet_core::event::RuntimeEvent::SubscriberLagged`], which is strictly better
//! than either blocking or silently dropping.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_core::event::{
    Event, EventBus, EventEnvelope, EventSubscriber, RuntimeEvent, topic_matches,
};
use tokio::sync::broadcast;

/// Default channel depth. Deep enough to absorb a burst of stream deltas, shallow enough
/// that a wedged subscriber is noticed within a turn rather than an hour.
const DEFAULT_CAPACITY: usize = 4096;

/// A `tokio::broadcast`-backed bus.
#[derive(Clone)]
pub struct BroadcastBus {
    tx: broadcast::Sender<EventEnvelope>,
    published: Arc<AtomicU64>,
}

impl BroadcastBus {
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Self {
            tx,
            published: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Total events published. Used in tests and in `rivet doctor`.
    #[must_use]
    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }

    /// Current subscriber count.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// A raw receiver, for the TUI and the JSONL writer.
    #[must_use]
    pub fn subscribe_raw(&self) -> broadcast::Receiver<EventEnvelope> {
        self.tx.subscribe()
    }

    /// Drive an [`EventSubscriber`] until the bus closes.
    ///
    /// Spawns a task; the returned handle aborts it on drop only if the caller drops it,
    /// so hold onto it for the lifetime of the plugin.
    pub fn attach(&self, subscriber: Arc<dyn EventSubscriber>) -> tokio::task::JoinHandle<()> {
        let mut rx = self.tx.subscribe();
        let bus = self.clone();
        tokio::spawn(async move {
            let topics = subscriber.topics();
            loop {
                match rx.recv().await {
                    Ok(envelope) => {
                        if topic_matches(&topics, envelope.topic()) {
                            subscriber.on_event(&envelope).await;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        // Report the loss rather than pretending it did not happen.
                        bus.publish(EventEnvelope::new(Event::Runtime(
                            RuntimeEvent::SubscriberLagged {
                                subscriber: subscriber.name().to_string(),
                                dropped,
                            },
                        )));
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }
}

impl Default for BroadcastBus {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for BroadcastBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BroadcastBus")
            .field("subscribers", &self.tx.receiver_count())
            .field("published", &self.published())
            .finish()
    }
}

impl EventBus for BroadcastBus {
    fn publish(&self, envelope: EventEnvelope) {
        self.published.fetch_add(1, Ordering::Relaxed);
        // `send` fails only when there are no receivers, which is normal and not an
        // error: nobody is listening, so nothing was lost.
        let _ = self.tx.send(envelope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::event::{AgentEvent, ToolEvent};
    use rivet_core::id::ToolCallId;
    use rivet_core::model::ModelId;
    use std::sync::Mutex;

    fn tool_event() -> EventEnvelope {
        EventEnvelope::new(Event::Tool(ToolEvent::Blocked {
            call_id: ToolCallId::new(),
            reason: "denied".into(),
        }))
    }

    fn agent_event() -> EventEnvelope {
        EventEnvelope::new(Event::Agent(AgentEvent::TextDelta { text: "hi".into() }))
    }

    #[tokio::test]
    async fn publishing_with_no_subscribers_is_not_an_error() {
        let bus = BroadcastBus::new();
        bus.publish(tool_event());
        assert_eq!(bus.published(), 1);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = BroadcastBus::new();
        let mut rx = bus.subscribe_raw();
        bus.publish(tool_event());
        let received = rx.recv().await.expect("event");
        assert_eq!(received.topic(), "tool.blocked");
    }

    #[derive(Debug)]
    struct Collector {
        seen: Mutex<Vec<String>>,
        filters: Vec<String>,
    }

    #[async_trait::async_trait]
    impl EventSubscriber for Collector {
        fn name(&self) -> &'static str {
            "collector"
        }

        fn topics(&self) -> Vec<String> {
            self.filters.clone()
        }

        async fn on_event(&self, envelope: &EventEnvelope) {
            self.seen.lock().unwrap().push(envelope.topic().to_string());
        }
    }

    #[tokio::test]
    async fn attached_subscribers_only_see_their_topics() {
        let bus = BroadcastBus::new();
        let collector = Arc::new(Collector {
            seen: Mutex::new(Vec::new()),
            filters: vec!["tool.".to_string()],
        });
        let handle = bus.attach(collector.clone());

        bus.publish(tool_event());
        bus.publish(agent_event());
        bus.publish(tool_event());

        // Let the subscriber task drain.
        tokio::task::yield_now().await;
        for _ in 0..50 {
            if collector.seen.lock().unwrap().len() >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }

        let seen = collector.seen.lock().unwrap().clone();
        assert_eq!(seen, vec!["tool.blocked", "tool.blocked"]);
        handle.abort();
    }

    #[tokio::test]
    async fn a_slow_subscriber_lags_instead_of_stalling_the_publisher() {
        // Capacity 2: the third publish evicts the first for a receiver that never reads.
        let bus = BroadcastBus::with_capacity(2);
        let mut slow = bus.subscribe_raw();

        for _ in 0..10 {
            bus.publish(tool_event());
        }
        assert_eq!(
            bus.published(),
            10,
            "publishing must not block on a slow reader"
        );

        match slow.recv().await {
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                assert!(
                    dropped >= 8,
                    "expected to be told about the drop, got {dropped}"
                );
            }
            other => panic!("expected a lag report, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_events_carry_correlation() {
        let bus = BroadcastBus::new();
        let mut rx = bus.subscribe_raw();
        let session = rivet_core::id::SessionId::new();
        let run = rivet_core::id::RunId::new();
        bus.publish(
            EventEnvelope::new(Event::Agent(AgentEvent::RequestStarted {
                model: ModelId::new("openai/gpt-4o").unwrap(),
                input_tokens_estimate: 100,
            }))
            .for_run(session, run),
        );
        let received = rx.recv().await.unwrap();
        assert_eq!(received.run_id, Some(run));
        assert_eq!(received.session_id, Some(session));
    }
}
