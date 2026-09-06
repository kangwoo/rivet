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

use crate::id::{AgentId, EventId, PluginId, RunId, SessionId, TaskId, ToolCallId};
use crate::model::{ModelId, StopReason, Usage};
use crate::policy::PolicyDecision;
use crate::task::TaskState;
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
    Task(TaskEvent),
    Plugin(PluginEvent),
    Runtime(RuntimeEvent),
}

impl Event {
    #[must_use]
    pub fn topic(&self) -> &'static str {
        match self {
            Self::Agent(e) => e.topic(),
            Self::Tool(e) => e.topic(),
            Self::Task(e) => e.topic(),
            Self::Plugin(e) => e.topic(),
            Self::Runtime(e) => e.topic(),
        }
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskEvent {
    Created {
        task_id: TaskId,
        goal: String,
    },
    StateChanged {
        task_id: TaskId,
        from: TaskState,
        to: TaskState,
        reason: String,
    },
    RunAttached {
        task_id: TaskId,
        run_id: RunId,
        attempt: u32,
    },
    ReviewRequested {
        task_id: TaskId,
        reviewer: AgentId,
    },
    ReviewCompleted {
        task_id: TaskId,
        verdict: crate::task::ReviewVerdict,
    },
}

impl TaskEvent {
    #[must_use]
    pub fn topic(&self) -> &'static str {
        match self {
            Self::Created { .. } => "task.created",
            Self::StateChanged { .. } => "task.state.changed",
            Self::RunAttached { .. } => "task.run.attached",
            Self::ReviewRequested { .. } => "task.review.requested",
            Self::ReviewCompleted { .. } => "task.review.completed",
        }
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
        let filters = vec!["tool.".to_string(), "task.state".to_string()];
        assert!(topic_matches(&filters, "tool.execute.started"));
        assert!(topic_matches(&filters, "task.state.changed"));
        assert!(!topic_matches(&filters, "agent.run.started"));
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
