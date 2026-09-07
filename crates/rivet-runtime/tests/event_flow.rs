//! What actually reaches the bus, and what it costs the loop.
//!
//! Three questions, in order of how much they would hurt to get wrong:
//!
//! 1. Is every topic in the vocabulary either published in this phase or deliberately
//!    deferred? (`every_bus_topic_is_claimed` — the list nothing can fall off.)
//! 2. Does a slow subscriber slow the loop? (Phase 3 `DoD` 2, measured.)
//! 3. Is a subscriber that falls behind actually *told on*? (`DoD` 3.)

mod support;

use std::sync::Arc;
use std::time::Duration;

use rivet_core::event::{AgentEvent, Event, EventEnvelope, EventSubscriber, JobEvent, PluginEvent};
use rivet_core::event::{EventBus, RuntimeEvent, ToolEvent};
use rivet_core::retry::ExponentialBackoff;
use rivet_runtime::agent_loop::{AgentLoop, RunConfig};
use rivet_runtime::jitter::NoJitter;
use rivet_runtime::{BroadcastBus, Drained};
use support::{
    EchoTool, FixtureModel, Harness, ProgressTool, Reply, sse_text_in_chunks, sse_tool_calls,
    transient,
};

fn agent_loop(harness: &Harness) -> AgentLoop {
    AgentLoop::new(
        harness.registry.clone(),
        harness.store.clone(),
        harness.bus.clone(),
        harness.assembler(),
        Arc::new(ExponentialBackoff {
            max_attempts: 3,
            base_delay_ms: 1,
            max_delay_ms: 5,
            factor: 2,
        }),
        Arc::new(NoJitter),
    )
}

fn config(harness: &Harness) -> RunConfig {
    let mut cfg = RunConfig::new(
        harness.agent(),
        harness.session_id,
        harness.workspace.clone(),
    );
    cfg.cancel_grace = Duration::from_millis(200);
    cfg
}

// --- 3.1: the vocabulary, and who owns each part of it -------------------------------------

/// Which phase publishes a topic.
#[derive(Debug, PartialEq, Eq)]
enum Owner {
    /// A publisher exists in this tree, as of Phase 3.
    Published,
    /// No publisher yet, and the phase that adds one.
    Deferred(u8),
}

/// Every topic is claimed by a phase, and a new one cannot slip in unclaimed.
///
/// The two `match`es below have no wildcard, in either layer. Add an `Event` *family* and
/// the outer one fails to compile; add a variant and the inner one does. Then the arm you
/// add has to name a phase, next to the list that says which topics this phase promises to
/// publish — which is the question the arm exists to make you answer.
///
/// `docs/architecture.md` §11-10 called the family hole "silent". This is the line that
/// closed it.
#[test]
fn every_bus_topic_is_claimed() {
    fn owner(event: &Event) -> Owner {
        match event {
            Event::Agent(agent) => match agent {
                AgentEvent::RunStarted { .. }
                | AgentEvent::TurnStarted { .. }
                | AgentEvent::RequestStarted { .. }
                | AgentEvent::TextDelta { .. }
                | AgentEvent::RequestCompleted { .. }
                | AgentEvent::RequestFailed { .. }
                | AgentEvent::TurnCompleted { .. }
                | AgentEvent::RunCompleted { .. } => Owner::Published,
            },
            Event::Tool(tool) => match tool {
                ToolEvent::Requested { .. }
                | ToolEvent::Started { .. }
                | ToolEvent::Progress { .. }
                | ToolEvent::Completed { .. }
                | ToolEvent::Blocked { .. } => Owner::Published,
                // The three lines `dispatch.rs` marks "4 Intercept, 5 Policy, 6 Approval".
                ToolEvent::PolicyEvaluated { .. }
                | ToolEvent::ApprovalRequested { .. }
                | ToolEvent::ApprovalResolved { .. } => Owner::Deferred(4),
            },
            // The whole family: there is no job runtime to publish from yet. The TUI's job
            // panel is built and tested against hand-made envelopes for exactly this reason.
            Event::Job(job) => match job {
                JobEvent::Created { .. }
                | JobEvent::StateChanged { .. }
                | JobEvent::RunAttached { .. }
                | JobEvent::ReviewRequested { .. }
                | JobEvent::ReviewCompleted { .. } => Owner::Deferred(5),
            },
            Event::Plugin(plugin) => match plugin {
                PluginEvent::Discovered { .. }
                | PluginEvent::Loaded { .. }
                | PluginEvent::LoadFailed { .. }
                | PluginEvent::Unloaded { .. } => Owner::Published,
            },
            Event::Runtime(runtime) => match runtime {
                RuntimeEvent::Started { .. }
                | RuntimeEvent::ShuttingDown { .. }
                | RuntimeEvent::SubscriberLagged { .. } => Owner::Published,
            },
        }
    }

    let samples = Event::one_of_each();
    let (published, deferred): (Vec<&Event>, Vec<&Event>) = samples
        .iter()
        .partition(|event| owner(event) == Owner::Published);
    let published: Vec<&str> = published.iter().map(|e| e.topic()).collect();
    let deferred: Vec<&str> = deferred.iter().map(|e| e.topic()).collect();

    assert_eq!(
        published,
        [
            "agent.run.started",
            "agent.turn.started",
            "agent.request.started",
            "agent.text.delta",
            "agent.request.completed",
            "agent.request.failed",
            "agent.turn.completed",
            "agent.run.completed",
            "tool.requested",
            "tool.execute.started",
            "tool.execute.progress",
            "tool.execute.completed",
            "tool.blocked",
            "plugin.discovered",
            "plugin.loaded",
            "plugin.load.failed",
            "plugin.unloaded",
            "runtime.started",
            "runtime.shutting_down",
            "runtime.subscriber.lagged",
        ],
        "a topic gained or lost a publisher; say which in `owner` and here"
    );
    assert_eq!(
        deferred,
        [
            "tool.policy.evaluated",
            "tool.approval.requested",
            "tool.approval.resolved",
            "job.created",
            "job.state.changed",
            "job.run.attached",
            "job.review.requested",
            "job.review.completed",
        ],
        "eight topics wait on Phase 4 (3) and Phase 5 (5)"
    );
}

#[tokio::test]
async fn a_run_publishes_every_agent_and_tool_topic_this_phase_owns() {
    // One script covering all thirteen: streamed text, a tool that reports progress, a
    // call outside the agent's scope (`tool.blocked`), and one transient failure the loop
    // retries (`agent.request.failed`).
    let harness = Harness::new().await;
    let (recorder, observer) = harness.recorder();

    // A text answer ends the run, so it comes last. The failure is first because the loop
    // retries it against the *next* script entry.
    let model = Arc::new(FixtureModel::new(vec![
        Reply::Fail(transient("the provider hiccupped")),
        Reply::Sse(sse_tool_calls(&[("with_progress", serde_json::json!({}))])),
        Reply::Sse(sse_tool_calls(&[(
            "echo",
            serde_json::json!({"message": "hi"}),
        )])),
        Reply::Sse(sse_text_in_chunks("thinking", 3)),
    ]));
    harness.register_model(model).await;
    harness.register_tool(Arc::new(ProgressTool)).await;
    harness.register_tool(Arc::new(EchoTool)).await;

    let mut cfg = config(&harness);
    // `echo` is registered but out of scope, so calling it is refused with `tool.blocked`
    // rather than failing to resolve.
    cfg.agent.tools = vec!["with_progress".into()];

    agent_loop(&harness)
        .run(
            cfg,
            harness.state().await,
            Some(rivet_core::model::Message::user("go")),
        )
        .await
        .expect("the run completes");

    assert_eq!(
        observer.drain_within(Duration::from_secs(2)).await,
        Drained::Complete
    );
    let seen = recorder.topics();
    for topic in [
        "agent.run.started",
        "agent.turn.started",
        "agent.request.started",
        "agent.text.delta",
        "agent.request.completed",
        "agent.request.failed",
        "agent.turn.completed",
        "agent.run.completed",
        "tool.requested",
        "tool.execute.started",
        "tool.execute.progress",
        "tool.execute.completed",
        "tool.blocked",
    ] {
        assert!(seen.iter().any(|s| s == topic), "{topic} missing: {seen:?}");
    }
}

#[tokio::test]
async fn a_host_lifecycle_publishes_every_runtime_and_plugin_topic_this_phase_owns() {
    // The host half of 3.1, without a loop: start, discover, load (one of them failing),
    // unload, shut down. `plugin.*` comes from the loader; `runtime.*` from `lifecycle`.
    let bus = BroadcastBus::new();
    let (recorder, observer) = {
        let recorder = Arc::new(support::Recorder::default());
        let observer = bus.observe(recorder.clone());
        (recorder, observer)
    };

    rivet_runtime::lifecycle::started(&bus, "0.1.0");
    bus.publish(EventEnvelope::new(Event::Plugin(PluginEvent::Discovered {
        plugin_id: rivet_core::id::PluginId::new("rivet.sample").unwrap(),
    })));
    bus.publish(EventEnvelope::new(Event::Plugin(PluginEvent::Loaded {
        plugin_id: rivet_core::id::PluginId::new("rivet.sample").unwrap(),
        capabilities: vec!["subscriber:sample".into()],
    })));
    bus.publish(EventEnvelope::new(Event::Plugin(PluginEvent::LoadFailed {
        plugin_id: rivet_core::id::PluginId::new("rivet.broken").unwrap(),
        error: "no".into(),
    })));
    rivet_runtime::lifecycle::shutting_down(&bus, "run finished");
    bus.publish(EventEnvelope::new(Event::Plugin(PluginEvent::Unloaded {
        plugin_id: rivet_core::id::PluginId::new("rivet.sample").unwrap(),
    })));

    observer.drain_within(Duration::from_secs(2)).await;
    assert_eq!(
        recorder.topics(),
        [
            "runtime.started",
            "plugin.discovered",
            "plugin.loaded",
            "plugin.load.failed",
            "runtime.shutting_down",
            "plugin.unloaded",
        ],
        "shutting_down comes before the unloads it explains"
    );
}

// --- DoD 2: a slow subscriber does not delay the loop ---------------------------------------

/// Sleeps for `per_event` on every event. Slower than any real renderer, on purpose.
#[derive(Debug)]
struct SleepySubscriber {
    per_event: Duration,
}

#[async_trait::async_trait]
impl EventSubscriber for SleepySubscriber {
    fn name(&self) -> &'static str {
        "sleepy"
    }

    async fn on_event(&self, _envelope: &EventEnvelope) {
        tokio::time::sleep(self.per_event).await;
    }
}

/// Phase 3 `DoD` 2, measured.
///
/// The bound is on the *loop*, deliberately, and not on the subscriber: a subscriber that
/// takes eight seconds to consume a run is allowed to, and that is the whole point. What
/// must not happen is the run waiting for it.
///
/// 400 deltas at 20 ms each is ≥ 8 s of subscriber work. The run is asserted to finish in
/// under 1 s — an 8× margin over serial delivery, wide enough that a loaded CI box does
/// not make it flaky and narrow enough that reintroducing backpressure fails it by a
/// factor of eight rather than by a hair.
#[tokio::test]
async fn a_slow_subscriber_does_not_delay_the_loop() {
    let harness = Harness::with_bus_capacity(16).await;
    let (recorder, recording) = harness.recorder();
    let _pump = harness.bus.observe(Arc::new(SleepySubscriber {
        per_event: Duration::from_millis(20),
    }));

    let model = Arc::new(FixtureModel::new(vec![Reply::Sse(sse_text_in_chunks(
        "word", 400,
    ))]));
    harness.register_model(model).await;

    let started = std::time::Instant::now();
    let summary = agent_loop(&harness)
        .run(
            config(&harness),
            harness.state().await,
            Some(rivet_core::model::Message::user("stream at me")),
        )
        .await
        .expect("the run completes");
    let elapsed = started.elapsed();

    assert_eq!(summary.turns, 1);
    assert!(
        elapsed < Duration::from_secs(1),
        "the loop waited on the subscriber: {elapsed:?} for 400 deltas at 20 ms each"
    );

    // And the loss was reported rather than absorbed: with capacity 16 and a subscriber
    // this slow, falling behind is certain. The report is published by the sleepy pump on
    // its own task, so it can arrive after the run returns — hence the poll rather than a
    // single look.
    let reported = wait_for_lag_from("sleepy", &recorder).await;
    recording.drain_within(Duration::from_secs(2)).await;
    assert!(
        reported,
        "no lag report for a subscriber that cannot possibly have kept up: {:?}",
        recorder.lag_reports()
    );
}

/// Wait for a lag report naming `subscriber`, up to a second.
///
/// Every pump reports on its own task, so "did the bus say so" is not a question a test
/// can ask synchronously the instant the publisher stops.
async fn wait_for_lag_from(subscriber: &str, recorder: &Arc<support::Recorder>) -> bool {
    for _ in 0..200 {
        if recorder
            .lag_reports()
            .iter()
            .any(|(name, dropped)| name == subscriber && *dropped > 0)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

// --- DoD 3: the lag report is a published event ----------------------------------------------

/// Holds its pump inside `on_event` until the test opens the gate.
///
/// Gated rather than permanently wedged, and that is not a convenience: tokio raises
/// `Lagged` from `recv`, so a subscriber that never returns from `on_event` never calls
/// `recv` again and is never told it lost anything. The loss is real; the *report* needs
/// the pump to come back. So the test wedges it, overflows the channel, then lets go.
#[derive(Debug)]
struct GatedSubscriber {
    open: tokio_util::sync::CancellationToken,
}

#[async_trait::async_trait]
impl EventSubscriber for GatedSubscriber {
    fn name(&self) -> &'static str {
        "wedged"
    }

    async fn on_event(&self, _envelope: &EventEnvelope) {
        self.open.cancelled().await;
    }
}

/// Phase 3 `DoD` 3.
///
/// The existing `a_slow_subscriber_lags_instead_of_stalling_the_publisher` asserts on the
/// receiver's `RecvError`, which is tokio's behavior rather than the bus's. This asserts
/// the thing the contract promises: a `runtime.subscriber.lagged` **event**, naming the
/// subscriber that lost events, delivered to everyone else on the bus.
#[tokio::test]
async fn a_lagging_subscriber_is_reported_by_name_on_the_bus() {
    let bus = BroadcastBus::with_capacity(8);
    let recorder = Arc::new(support::Recorder::default());
    let recording = bus.observe(recorder.clone());
    let gate = tokio_util::sync::CancellationToken::new();
    let _wedged = bus.observe(Arc::new(GatedSubscriber { open: gate.clone() }));

    // Both receivers exist from `observe`, so the backlog builds whether or not the pumps
    // have been scheduled yet. Yielding once makes the wedged one hold an event rather
    // than a queue, which is the shape the assertion describes.
    tokio::task::yield_now().await;

    for index in 0..40 {
        bus.publish(EventEnvelope::new(Event::Agent(AgentEvent::TextDelta {
            text: index.to_string(),
        })));
    }
    gate.cancel();

    let reported = wait_for_lag_from("wedged", &recorder).await;
    recording.drain_within(Duration::from_secs(2)).await;
    assert!(
        reported,
        "the bus did not name the subscriber that lost events: {:?}",
        recorder.lag_reports()
    );
}

#[tokio::test]
async fn runtime_started_precedes_everything_it_would_describe() {
    // `--jsonl`'s first line. Today's ordering puts the observer on after `catalog::load`,
    // which means the whole plugin lifecycle happens in an empty room.
    let bus = BroadcastBus::new();
    let recorder = Arc::new(support::Recorder::default());
    let observer = bus.observe(recorder.clone());

    rivet_runtime::lifecycle::started(&bus, "0.1.0");
    bus.publish(EventEnvelope::new(Event::Plugin(PluginEvent::Discovered {
        plugin_id: rivet_core::id::PluginId::new("rivet.sample").unwrap(),
    })));

    observer.drain_within(Duration::from_secs(2)).await;
    let topics = recorder.topics();
    assert_eq!(topics.first().map(String::as_str), Some("runtime.started"));
    assert!(
        topics
            .iter()
            .position(|t| t == "runtime.started")
            .zip(topics.iter().position(|t| t == "plugin.discovered"))
            .is_some_and(|(started, discovered)| started < discovered)
    );
}
