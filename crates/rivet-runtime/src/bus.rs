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
use std::time::Duration;

use rivet_core::event::{
    Event, EventBus, EventEnvelope, EventSubscriber, RuntimeEvent, topic_matches,
};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

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

    /// Drive an [`EventSubscriber`] until the bus closes. The plugin pump.
    ///
    /// Spawns a task; the returned handle aborts it on drop only if the caller drops it,
    /// so hold onto it for the lifetime of the plugin. The registry keeps it under the
    /// owning instance, which is what lets `unregister_all` stop an unloaded plugin's
    /// observation.
    pub fn attach(&self, subscriber: Arc<dyn EventSubscriber>) -> JoinHandle<()> {
        let rx = self.tx.subscribe();
        let bus = self.clone();
        tokio::spawn(pump(rx, subscriber, bus, None))
    }

    /// Drive an [`EventSubscriber`] with a handle that can end it. The host pump.
    ///
    /// Same delivery and the same lag reporting as [`attach`](BroadcastBus::attach) —
    /// one pump serves both, so a host renderer that falls behind is reported by name
    /// exactly as a plugin is. What differs is only that the host can say "stop, but
    /// finish what is already published" ([`Observer::drain_within`]).
    ///
    /// The host's own consumers (`--jsonl`, the TUI) are not plugins and do not pass
    /// through a grant: what constrains them is the person who ran the CLI, not a profile
    /// (`docs/architecture.md` §11-10).
    pub fn observe(&self, subscriber: Arc<dyn EventSubscriber>) -> Observer {
        let rx = self.tx.subscribe();
        let bus = self.clone();
        let stop = CancellationToken::new();
        let handle = tokio::spawn(pump(rx, subscriber, bus, Some(stop.clone())));
        Observer {
            stop,
            handle: Some(handle),
        }
    }
}

/// A running host pump, with a way to end it.
#[derive(Debug)]
pub struct Observer {
    stop: CancellationToken,
    handle: Option<JoinHandle<()>>,
}

impl Observer {
    /// Deliver everything published so far, then end. Call it after publishing is done.
    ///
    /// [`EventBus::publish`] is synchronous, so "the channel is empty" and "everything
    /// published so far has been handed over" are the same statement — which is why this
    /// needs no sentinel event. A sentinel would be worse than nothing here: the last
    /// events of a run are `plugin.unloaded`, published *after* `runtime.shutting_down`,
    /// so a pump that stopped at the sentinel would cut off exactly the tail an operator
    /// is reading the stream for.
    ///
    /// `Truncated` means the budget ran out with events still undelivered. The caller
    /// should say so rather than let a consumer believe it read the whole stream.
    pub async fn drain_within(mut self, budget: Duration) -> Drained {
        self.stop.cancel();
        let Some(mut handle) = self.handle.take() else {
            return Drained::Complete;
        };
        // `&mut handle` rather than `handle`: a timeout that consumed it would *detach* the
        // pump, not stop it. `Drop` cannot pick that up either -- `take` has already emptied
        // the field -- so the task would go on holding a receiver and delivering into a
        // renderer nobody is reading. In the CLI the process exits a moment later and it
        // does not show; an embedder that drains once per run leaks one task per truncation.
        if tokio::time::timeout(budget, &mut handle).await.is_ok() {
            Drained::Complete
        } else {
            handle.abort();
            Drained::Truncated {
                budget_ms: u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
            }
        }
    }
}

/// Dropping an observer without draining aborts its pump, so an early return on an error
/// path cannot leave a task delivering into a renderer nobody is reading.
impl Drop for Observer {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

/// What [`Observer::drain_within`] found when the budget ran out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drained {
    /// Everything published had been delivered.
    Complete,
    /// The budget expired first. The stream's tail is missing.
    Truncated { budget_ms: u64 },
}

/// One subscriber's delivery loop, shared by [`BroadcastBus::attach`] and
/// [`BroadcastBus::observe`].
///
/// One function rather than two so that the lag report cannot drift between the plugin
/// path and the host path — a host renderer that silently missed events while a plugin's
/// misses were reported would make the stream lie in exactly the mode meant for reading it.
///
/// `topics` is read once, before the first event: a subscriber cannot widen its own filter
/// after registration.
async fn pump(
    mut rx: broadcast::Receiver<EventEnvelope>,
    subscriber: Arc<dyn EventSubscriber>,
    bus: BroadcastBus,
    stop: Option<CancellationToken>,
) {
    let topics = subscriber.topics();
    let mut gap = Gap::default();
    loop {
        let received = match &stop {
            None => rx.recv().await,
            // Biased: whatever is already in the channel is delivered before the stop is
            // noticed, so `drain_within` does not have to race the pump for the backlog.
            Some(token) => tokio::select! {
                biased;
                result = rx.recv() => result,
                () = token.cancelled() => break,
            },
        };
        match received {
            Ok(envelope) => {
                gap.closed();
                deliver(&subscriber, &topics, &envelope).await;
            }
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                gap.report(&bus, subscriber.name(), dropped);
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }

    // Stopped. Hand over what was already published and end.
    loop {
        match rx.try_recv() {
            Ok(envelope) => {
                gap.closed();
                deliver(&subscriber, &topics, &envelope).await;
            }
            Err(broadcast::error::TryRecvError::Lagged(dropped)) => {
                gap.report(&bus, subscriber.name(), dropped);
            }
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                return;
            }
        }
    }
}

/// One lag report per gap — not one per `Lagged`.
///
/// The difference is load-bearing, and it is not a nicety. Reporting is itself a
/// `publish`, and a publish onto a full channel evicts the oldest slot — which, right
/// after a `Lagged`, is exactly where tokio has just reset this receiver. So a report per
/// `Lagged` **feeds itself**: report, lag by one, report, lag by one, without ever
/// delivering anything. Measured on a capacity-8 bus with two subscribers behind it: 40
/// published events became 118,312 before the pump gave up any of them.
///
/// A gap ends when something is actually delivered. Drops counted after the report and
/// before that delivery go unreported, and they should: they are the ones the report
/// caused.
#[derive(Debug, Default)]
struct Gap {
    announced: bool,
}

impl Gap {
    fn report(&mut self, bus: &BroadcastBus, subscriber: &str, dropped: u64) {
        if self.announced {
            return;
        }
        self.announced = true;
        bus.publish(EventEnvelope::new(Event::Runtime(
            RuntimeEvent::SubscriberLagged {
                subscriber: subscriber.to_string(),
                dropped,
            },
        )));
    }

    fn closed(&mut self) {
        self.announced = false;
    }
}

async fn deliver(subscriber: &Arc<dyn EventSubscriber>, topics: &[String], env: &EventEnvelope) {
    if topic_matches(topics, env.topic()) {
        subscriber.on_event(env).await;
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
    async fn an_observer_delivers_the_backlog_before_it_stops() {
        // What `--jsonl` depends on: the tail of the stream is the plugin teardown, and it
        // is published *after* the loop is done. A pump that stopped on the stop signal
        // rather than on an empty channel would drop exactly those lines.
        let bus = BroadcastBus::new();
        let collector = Arc::new(Collector {
            seen: Mutex::new(Vec::new()),
            filters: Vec::new(),
        });
        let observer = bus.observe(collector.clone());

        for _ in 0..64 {
            bus.publish(tool_event());
        }
        let drained = observer
            .drain_within(std::time::Duration::from_secs(2))
            .await;

        assert_eq!(drained, Drained::Complete);
        assert_eq!(collector.seen.lock().unwrap().len(), 64);
    }

    #[tokio::test]
    async fn a_drain_that_runs_out_of_budget_says_so() {
        // A subscriber that never returns from `on_event` cannot be drained, and the
        // caller has to be able to tell that from a stream that simply ended.
        #[derive(Debug)]
        struct Wedged;

        #[async_trait::async_trait]
        impl EventSubscriber for Wedged {
            fn name(&self) -> &'static str {
                "wedged"
            }

            async fn on_event(&self, _envelope: &EventEnvelope) {
                std::future::pending::<()>().await;
            }
        }

        let bus = BroadcastBus::new();
        let observer = bus.observe(Arc::new(Wedged));
        bus.publish(tool_event());

        let drained = observer
            .drain_within(std::time::Duration::from_millis(50))
            .await;
        assert_eq!(drained, Drained::Truncated { budget_ms: 50 });
    }

    #[tokio::test]
    async fn a_drain_that_runs_out_of_budget_still_ends_the_pump() {
        // A timeout that *consumed* the handle would detach the task rather than stop it,
        // and `Drop` cannot clean up after that -- `drain_within` has already taken the
        // handle out. The pump would keep its receiver and keep delivering into a renderer
        // whose caller has moved on. Held by the subscriber's own refcount: the pump owns
        // the only other `Arc`, so it falling back to one means the task is gone.
        #[derive(Debug)]
        struct Wedged;

        #[async_trait::async_trait]
        impl EventSubscriber for Wedged {
            fn name(&self) -> &'static str {
                "wedged"
            }

            async fn on_event(&self, _envelope: &EventEnvelope) {
                std::future::pending::<()>().await;
            }
        }

        let bus = BroadcastBus::new();
        let subscriber = Arc::new(Wedged);
        let observer = bus.observe(subscriber.clone());
        bus.publish(tool_event());
        assert_eq!(
            observer
                .drain_within(std::time::Duration::from_millis(50))
                .await,
            Drained::Truncated { budget_ms: 50 }
        );

        // `abort` schedules the drop rather than performing it, so this waits for it.
        for _ in 0..200 {
            if Arc::strong_count(&subscriber) == 1 && bus.subscriber_count() == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        panic!(
            "the truncated pump was detached, not stopped: {} refs, {} receivers",
            Arc::strong_count(&subscriber),
            bus.subscriber_count()
        );
    }

    #[tokio::test]
    async fn an_observer_honors_its_own_topic_filter() {
        let bus = BroadcastBus::new();
        let collector = Arc::new(Collector {
            seen: Mutex::new(Vec::new()),
            filters: vec!["tool.".to_string()],
        });
        let observer = bus.observe(collector.clone());

        bus.publish(tool_event());
        bus.publish(agent_event());
        observer
            .drain_within(std::time::Duration::from_secs(2))
            .await;

        assert_eq!(*collector.seen.lock().unwrap(), vec!["tool.blocked"]);
    }

    #[tokio::test]
    async fn a_lag_report_does_not_feed_itself_into_a_runaway() {
        // The report is a publish, and a publish onto a full channel evicts the slot a
        // just-lagged receiver was reset to. One report per `Lagged` therefore causes the
        // next `Lagged`: before `Gap`, 40 events on this bus became 118,312 with nothing
        // ever delivered. The bound below is generous — what it rules out is unbounded.
        let bus = BroadcastBus::with_capacity(8);
        let a = Arc::new(Collector {
            seen: Mutex::new(Vec::new()),
            filters: Vec::new(),
        });
        let b = Arc::new(Collector {
            seen: Mutex::new(Vec::new()),
            filters: Vec::new(),
        });
        let one = bus.observe(a.clone());
        let two = bus.observe(b.clone());

        for _ in 0..40 {
            bus.publish(tool_event());
        }
        one.drain_within(std::time::Duration::from_secs(2)).await;
        two.drain_within(std::time::Duration::from_secs(2)).await;

        assert!(
            bus.published() < 100,
            "the lag reports fed each other: {} events from 40 publishes",
            bus.published()
        );
        assert!(
            !a.seen.lock().unwrap().is_empty(),
            "a subscriber that only ever reports its own lag delivers nothing"
        );
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
