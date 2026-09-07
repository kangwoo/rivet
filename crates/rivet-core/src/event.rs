//! The event bus: the primary extension point.
//!
//! Two rules make this workable:
//!
//! 1. **Bus events are observational.** Subscribers see what happened; they cannot mutate
//!    the runtime by returning a value. Interception is a separate, explicit contract
//!    ([`crate::plugin::Interceptor`]) so that "who can block a tool call" is an
//!    enumerable list rather than "any subscriber".
//! 2. **A slow subscriber must never stall the agent.** Delivery is lossy by design; the
//!    bus reports drops rather than applying backpressure to the loop.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::id::{AgentId, EventId, JobId, PluginId, RunId, SessionId, ToolCallId};
use crate::job::JobState;
use crate::model::{ModelId, StopReason, Usage};
use crate::policy::PolicyDecision;
use crate::time::Timestamp;

/// An event plus its delivery metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub id: EventId,
    pub at: Timestamp,
    /// Correlation: which run produced this. `None` for runtime-level events.
    pub run_id: Option<RunId>,
    pub session_id: Option<SessionId>,
    pub payload: Event,
}

impl EventEnvelope {
    #[must_use]
    pub fn new(payload: Event) -> Self {
        Self {
            id: EventId::new(),
            at: Timestamp::now(),
            run_id: None,
            session_id: None,
            payload,
        }
    }

    #[must_use]
    pub fn for_run(mut self, session_id: SessionId, run_id: RunId) -> Self {
        self.session_id = Some(session_id);
        self.run_id = Some(run_id);
        self
    }

    /// Dotted name (`tool.execute.completed`) used for subscription filters.
    #[must_use]
    pub fn topic(&self) -> &'static str {
        self.payload.topic()
    }
}

/// Everything that happens in the runtime.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Agent(AgentEvent),
    Tool(ToolEvent),
    Job(JobEvent),
    Plugin(PluginEvent),
    Runtime(RuntimeEvent),
}

impl Event {
    /// The topic this event publishes on.
    ///
    /// **Adding a variant here adds a topic *family*, and the narrowed profiles will not
    /// receive it.** `Profile::subscribable_topics` in `rivet-cli` enumerates seven
    /// prefixes — `agent.request.`, `agent.run.`, `agent.turn.`, `job.`, `plugin.`,
    /// `runtime.`, `tool.` — because prefixes cannot express "everything but
    /// `agent.text`". A sixth family falls outside all seven and would be denied to
    /// `readonly`, `reviewer` and `production` with nothing failing.
    ///
    /// That was the safe direction and the silent one until Phase 3, which made
    /// [`Event::one_of_each`] the sample every family check folds over. Two
    /// wildcard-free `match`es now stand where this note used to: `rivet-cli`'s
    /// `every_topic_is_granted_or_deliberately_withheld` and `rivet-runtime`'s
    /// `every_bus_topic_is_claimed`. A new family fails to compile at both.
    #[must_use]
    pub fn topic(&self) -> &'static str {
        match self {
            Self::Agent(e) => e.topic(),
            Self::Tool(e) => e.topic(),
            Self::Job(e) => e.topic(),
            Self::Plugin(e) => e.topic(),
            Self::Runtime(e) => e.topic(),
        }
    }

    /// One value per variant of every family, so a caller can enumerate the whole topic
    /// space without a wildcard.
    ///
    /// Values rather than topic strings. The compile-time tripwire lives in the callers'
    /// wildcard-free `match`es, and a `match` needs values — hand out `&'static str` and
    /// today's compile error becomes tomorrow's runtime surprise.
    ///
    /// This function is a `vec![]` literal, so it cannot itself notice a missing variant.
    /// What notices is the sequence: add a variant, a caller's `match` fails to compile,
    /// add the arm, the arm's topic is absent from the caller's expected list, the test
    /// fails, come back here. `every_sample_has_a_distinct_topic` catches only the
    /// copy-paste duplicate.
    #[must_use]
    pub fn one_of_each() -> Vec<Self> {
        let mut all: Vec<Self> = AgentEvent::one_of_each()
            .into_iter()
            .map(Self::Agent)
            .collect();
        all.extend(ToolEvent::one_of_each().into_iter().map(Self::Tool));
        all.extend(JobEvent::one_of_each().into_iter().map(Self::Job));
        all.extend(PluginEvent::one_of_each().into_iter().map(Self::Plugin));
        all.extend(RuntimeEvent::one_of_each().into_iter().map(Self::Runtime));
        all
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    RunStarted {
        agent_id: AgentId,
        model: ModelId,
    },
    TurnStarted {
        turn: u32,
    },
    RequestStarted {
        model: ModelId,
        input_tokens_estimate: u64,
    },
    /// Streaming text, for live UI. Not persisted — the session log stores the assembled
    /// message instead, so a replay does not have to re-derive it from deltas.
    TextDelta {
        text: String,
    },
    RequestCompleted {
        usage: Usage,
        stop_reason: StopReason,
        latency_ms: u64,
    },
    RequestFailed {
        error: String,
        will_retry: bool,
        attempt: u32,
    },
    TurnCompleted {
        turn: u32,
    },
    RunCompleted {
        turns: u32,
        stop: crate::agent::StopReason,
    },
}

impl AgentEvent {
    /// The topic this event publishes on.
    ///
    /// Topics are also what [`crate::capability::Permission::EventsSubscribe`] scopes
    /// over, and the narrowed profiles are granted by *enumerating* prefixes rather than
    /// excluding one — prefixes cannot express "not". **So a topic added here is not
    /// granted to `readonly`, `reviewer` or `production` until somebody adds it to
    /// `Profile::subscribable_topics` in `rivet-cli`.** That is the safe direction and the
    /// silent one, which is why the warning is at the line you would be editing.
    ///
    /// `rivet-cli`'s `every_topic_is_granted_or_deliberately_withheld` makes adding a
    /// variant here a compile error at the list that decides it. Since Phase 3 the same
    /// test covers a whole new *family* on [`Event`] too — see the note there.
    #[must_use]
    pub fn topic(&self) -> &'static str {
        match self {
            Self::RunStarted { .. } => "agent.run.started",
            Self::TurnStarted { .. } => "agent.turn.started",
            Self::RequestStarted { .. } => "agent.request.started",
            Self::TextDelta { .. } => "agent.text.delta",
            Self::RequestCompleted { .. } => "agent.request.completed",
            Self::RequestFailed { .. } => "agent.request.failed",
            Self::TurnCompleted { .. } => "agent.turn.completed",
            Self::RunCompleted { .. } => "agent.run.completed",
        }
    }

    /// One value per variant, so a caller can enumerate the topics without a wildcard.
    ///
    /// Exists for `rivet-cli`'s check that every agent topic is either granted to the
    /// narrowed profiles or deliberately withheld: adding a variant has to break that
    /// test rather than quietly fall outside the grant. [`Event::one_of_each`] gathers
    /// this and its four siblings so the same check covers every family.
    ///
    /// A `vec!` literal is not exhaustiveness-checked, so the list alone would let a new
    /// variant be answered for in that test and still never be *tested* — the test's own
    /// `match` would compile once the answer was written, and the topic it claims to grant
    /// would go unchecked. The wildcard-free `match` below is what makes the list itself
    /// a compile error to forget.
    #[must_use]
    pub fn one_of_each() -> Vec<Self> {
        use crate::model::{StopReason, Usage};
        let all = vec![
            Self::RunStarted {
                agent_id: crate::id::AgentId::new(),
                model: crate::model::ModelId::new("p/m").expect("valid"),
            },
            Self::TurnStarted { turn: 1 },
            Self::RequestStarted {
                model: crate::model::ModelId::new("p/m").expect("valid"),
                input_tokens_estimate: 0,
            },
            Self::TextDelta {
                text: String::new(),
            },
            Self::RequestCompleted {
                usage: Usage::default(),
                stop_reason: StopReason::EndTurn,
                latency_ms: 0,
            },
            Self::RequestFailed {
                error: String::new(),
                will_retry: false,
                attempt: 1,
            },
            Self::TurnCompleted { turn: 1 },
            Self::RunCompleted {
                turns: 1,
                stop: crate::agent::StopReason::EndTurn,
            },
        ];
        for event in &all {
            // No wildcard: a new variant fails to compile *here*, one line below the list
            // it has to be added to.
            match event {
                Self::RunStarted { .. }
                | Self::TurnStarted { .. }
                | Self::RequestStarted { .. }
                | Self::TextDelta { .. }
                | Self::RequestCompleted { .. }
                | Self::RequestFailed { .. }
                | Self::TurnCompleted { .. }
                | Self::RunCompleted { .. } => {}
            }
        }
        all
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolEvent {
    Requested {
        call_id: ToolCallId,
        name: String,
    },
    PolicyEvaluated {
        call_id: ToolCallId,
        /// Boxed: every event is cloned onto the broadcast bus, and a `PolicyDecision`
        /// carries a rewritten `ToolCall` plus constraints. Inlining it would make the
        /// whole `Event` enum as large as its fattest variant, on every text delta.
        decision: Box<PolicyDecision>,
        policy: String,
    },
    ApprovalRequested {
        call_id: ToolCallId,
        reason: String,
    },
    ApprovalResolved {
        call_id: ToolCallId,
        approved: bool,
    },
    Started {
        call_id: ToolCallId,
        name: String,
        sandboxed: bool,
    },
    /// Incremental output from a long-running tool, for live UI.
    Progress {
        call_id: ToolCallId,
        message: String,
    },
    Completed {
        call_id: ToolCallId,
        is_error: bool,
        duration_ms: u64,
    },
    Blocked {
        call_id: ToolCallId,
        reason: String,
    },
}

impl ToolEvent {
    #[must_use]
    pub fn topic(&self) -> &'static str {
        match self {
            Self::Requested { .. } => "tool.requested",
            Self::PolicyEvaluated { .. } => "tool.policy.evaluated",
            Self::ApprovalRequested { .. } => "tool.approval.requested",
            Self::ApprovalResolved { .. } => "tool.approval.resolved",
            Self::Started { .. } => "tool.execute.started",
            Self::Progress { .. } => "tool.execute.progress",
            Self::Completed { .. } => "tool.execute.completed",
            Self::Blocked { .. } => "tool.blocked",
        }
    }

    /// One value per variant. See [`Event::one_of_each`].
    #[must_use]
    pub fn one_of_each() -> Vec<Self> {
        vec![
            Self::Requested {
                call_id: ToolCallId::new(),
                name: "t".into(),
            },
            Self::PolicyEvaluated {
                call_id: ToolCallId::new(),
                decision: Box::new(PolicyDecision::allow()),
                policy: "p".into(),
            },
            Self::ApprovalRequested {
                call_id: ToolCallId::new(),
                reason: String::new(),
            },
            Self::ApprovalResolved {
                call_id: ToolCallId::new(),
                approved: true,
            },
            Self::Started {
                call_id: ToolCallId::new(),
                name: "t".into(),
                sandboxed: false,
            },
            Self::Progress {
                call_id: ToolCallId::new(),
                message: String::new(),
            },
            Self::Completed {
                call_id: ToolCallId::new(),
                is_error: false,
                duration_ms: 0,
            },
            Self::Blocked {
                call_id: ToolCallId::new(),
                reason: String::new(),
            },
        ]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobEvent {
    Created {
        job_id: JobId,
        goal: String,
    },
    StateChanged {
        job_id: JobId,
        from: JobState,
        to: JobState,
        reason: String,
    },
    RunAttached {
        job_id: JobId,
        run_id: RunId,
        attempt: u32,
    },
    ReviewRequested {
        job_id: JobId,
        reviewer: AgentId,
    },
    ReviewCompleted {
        job_id: JobId,
        verdict: crate::job::ReviewVerdict,
    },
}

impl JobEvent {
    #[must_use]
    pub fn topic(&self) -> &'static str {
        match self {
            Self::Created { .. } => "job.created",
            Self::StateChanged { .. } => "job.state.changed",
            Self::RunAttached { .. } => "job.run.attached",
            Self::ReviewRequested { .. } => "job.review.requested",
            Self::ReviewCompleted { .. } => "job.review.completed",
        }
    }

    /// One value per variant. See [`Event::one_of_each`].
    #[must_use]
    pub fn one_of_each() -> Vec<Self> {
        vec![
            Self::Created {
                job_id: JobId::new(),
                goal: String::new(),
            },
            Self::StateChanged {
                job_id: JobId::new(),
                from: JobState::Pending,
                to: JobState::Ready,
                reason: String::new(),
            },
            Self::RunAttached {
                job_id: JobId::new(),
                run_id: RunId::new(),
                attempt: 1,
            },
            Self::ReviewRequested {
                job_id: JobId::new(),
                reviewer: AgentId::new(),
            },
            Self::ReviewCompleted {
                job_id: JobId::new(),
                verdict: crate::job::ReviewVerdict::Approve,
            },
        ]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginEvent {
    Discovered {
        plugin_id: PluginId,
    },
    Loaded {
        plugin_id: PluginId,
        capabilities: Vec<String>,
    },
    LoadFailed {
        plugin_id: PluginId,
        error: String,
    },
    Unloaded {
        plugin_id: PluginId,
    },
}

impl PluginEvent {
    #[must_use]
    pub fn topic(&self) -> &'static str {
        match self {
            Self::Discovered { .. } => "plugin.discovered",
            Self::Loaded { .. } => "plugin.loaded",
            Self::LoadFailed { .. } => "plugin.load.failed",
            Self::Unloaded { .. } => "plugin.unloaded",
        }
    }

    /// One value per variant. See [`Event::one_of_each`].
    ///
    /// # Panics
    /// Never: `p.sample` is a valid plugin id and `every_sample_has_a_distinct_topic`
    /// constructs the whole list.
    #[must_use]
    pub fn one_of_each() -> Vec<Self> {
        let plugin_id = PluginId::new("p.sample").expect("a literal plugin id is valid");
        vec![
            Self::Discovered {
                plugin_id: plugin_id.clone(),
            },
            Self::Loaded {
                plugin_id: plugin_id.clone(),
                capabilities: Vec::new(),
            },
            Self::LoadFailed {
                plugin_id: plugin_id.clone(),
                error: String::new(),
            },
            Self::Unloaded { plugin_id },
        ]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeEvent {
    Started {
        version: String,
    },
    ShuttingDown {
        reason: String,
    },
    /// The bus dropped events for a subscriber that could not keep up. Emitting this
    /// rather than blocking is the deliberate tradeoff — but it must be *visible*, or a
    /// telemetry plugin will silently under-report.
    SubscriberLagged {
        subscriber: String,
        dropped: u64,
    },
}

impl RuntimeEvent {
    #[must_use]
    pub fn topic(&self) -> &'static str {
        match self {
            Self::Started { .. } => "runtime.started",
            Self::ShuttingDown { .. } => "runtime.shutting_down",
            Self::SubscriberLagged { .. } => "runtime.subscriber.lagged",
        }
    }

    /// One value per variant. See [`Event::one_of_each`].
    #[must_use]
    pub fn one_of_each() -> Vec<Self> {
        vec![
            Self::Started {
                version: String::new(),
            },
            Self::ShuttingDown {
                reason: String::new(),
            },
            Self::SubscriberLagged {
                subscriber: String::new(),
                dropped: 0,
            },
        ]
    }
}

/// Publish side of the bus. Handed to tools and plugins.
///
/// `publish` is deliberately synchronous and infallible: an agent loop must never await,
/// or fail, on telemetry.
pub trait EventBus: Send + Sync + fmt::Debug {
    fn publish(&self, envelope: EventEnvelope);
}

/// Subscribe side. Only the runtime hands this out, and only to plugins that declared
/// `events.subscribe`.
#[async_trait]
pub trait EventSubscriber: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;

    /// Topic prefixes this subscriber wants (`tool.` matches `tool.execute.started`).
    /// Empty means everything.
    fn topics(&self) -> Vec<String> {
        Vec::new()
    }

    /// Handle one event. Must return promptly; a slow handler causes drops, reported as
    /// [`RuntimeEvent::SubscriberLagged`].
    async fn on_event(&self, envelope: &EventEnvelope);
}

/// Whether a topic filter matches a topic.
#[must_use]
pub fn topic_matches(filters: &[String], topic: &str) -> bool {
    filters.is_empty() || filters.iter().any(|f| topic.starts_with(f.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_are_stable_dotted_names() {
        let ev = Event::Tool(ToolEvent::Blocked {
            call_id: ToolCallId::new(),
            reason: "denied".into(),
        });
        assert_eq!(ev.topic(), "tool.blocked");
    }

    #[test]
    fn empty_filter_subscribes_to_everything() {
        assert!(topic_matches(&[], "anything.at.all"));
    }

    #[test]
    fn prefix_filters_match_by_segment_prefix() {
        let filters = vec!["tool.".to_string(), "job.state".to_string()];
        assert!(topic_matches(&filters, "tool.execute.started"));
        assert!(topic_matches(&filters, "job.state.changed"));
        assert!(!topic_matches(&filters, "agent.run.started"));
    }

    /// `one_of_each` is a literal list, so the failure it *can* have is a copy-paste
    /// duplicate — two entries for one variant, and a variant with none. The tripwire for
    /// a missing variant lives in the wildcard-free `match`es that consume this; see
    /// [`Event::one_of_each`].
    #[test]
    fn every_sample_has_a_distinct_topic() {
        let samples = Event::one_of_each();
        let mut topics: Vec<&str> = samples.iter().map(Event::topic).collect();
        let count = topics.len();
        topics.sort_unstable();
        topics.dedup();
        assert_eq!(
            topics.len(),
            count,
            "two samples share a topic, so one variant has none: {topics:?}"
        );
    }

    #[test]
    fn envelope_carries_correlation_ids() {
        let session = SessionId::new();
        let run = RunId::new();
        let env = EventEnvelope::new(Event::Runtime(RuntimeEvent::Started {
            version: "0.1.0".into(),
        }))
        .for_run(session, run);
        assert_eq!(env.session_id, Some(session));
        assert_eq!(env.run_id, Some(run));
        assert_eq!(env.topic(), "runtime.started");
    }
}
