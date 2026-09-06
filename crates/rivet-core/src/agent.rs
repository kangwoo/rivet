//! The agent: a configured execution unit.
//!
//! An agent owns almost nothing. It is a *binding* of a model, a tool scope, a set of
//! context providers, and limits. State lives in the session; intent lives in the job.
//! Keeping the agent thin is what lets one job be carried by several runs, possibly on
//! different models.

use serde::{Deserialize, Serialize};

use crate::id::AgentId;
use crate::model::ModelId;

/// Configuration for an agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentSpec {
    pub id: AgentId,
    /// Human name: `coder`, `reviewer`, `planner`.
    pub name: String,
    pub model: ModelId,
    /// Base instructions. Providers see this as the system prompt's first slot.
    pub instructions: String,
    /// Tool names this agent may use. Empty means "every registered tool", which is only
    /// appropriate for an interactive developer agent.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Context providers, by name, in assembly order.
    #[serde(default)]
    pub context_providers: Vec<String>,
    pub limits: RunLimits,
}

impl AgentSpec {
    pub fn new(name: impl Into<String>, model: ModelId) -> Self {
        Self {
            id: AgentId::new(),
            name: name.into(),
            model,
            instructions: String::new(),
            tools: Vec::new(),
            context_providers: Vec::new(),
            limits: RunLimits::default(),
        }
    }

    /// Whether `tool` is in scope for this agent.
    #[must_use]
    pub fn allows_tool(&self, tool: &str) -> bool {
        self.tools.is_empty() || self.tools.iter().any(|t| t == tool)
    }
}

/// Hard stops on a run.
///
/// Every one of these exists because its absence has burned someone: an agent that loops
/// forever, one that spends $400 on a typo, one that hangs a CI job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLimits {
    /// Model round-trips. A turn is one request plus any tool calls it triggers.
    pub max_turns: u32,
    /// Wall clock for the whole run.
    pub max_duration_ms: u64,
    /// Cumulative tokens across the run.
    pub max_total_tokens: u64,
    /// Tokens the assembled request may occupy.
    pub max_context_tokens: u32,
    /// Consecutive tool failures before giving up. Catches the "retry the same broken
    /// command 40 times" pattern.
    pub max_consecutive_tool_errors: u32,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_turns: 50,
            max_duration_ms: 30 * 60 * 1000,
            max_total_tokens: 2_000_000,
            max_context_tokens: 128_000,
            max_consecutive_tool_errors: 5,
        }
    }
}

/// Why a run ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stop", rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished without requesting tools. The normal ending.
    EndTurn,
    /// A limit from [`RunLimits`] tripped. Carries which one, because "it just stopped"
    /// is the least useful thing a runtime can say.
    LimitReached { limit: LimitKind },
    /// Cooperative cancellation.
    Cancelled,
    /// A policy denied something the run could not proceed without.
    PolicyBlocked { reason: String },
    /// Unrecoverable error.
    Error { message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    Turns,
    Duration,
    Tokens,
    ContextSize,
    ConsecutiveToolErrors,
}

impl StopReason {
    /// Whether the run ended having done what was asked.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::EndTurn)
    }
}

/// A completed run, the unit an [`crate::memory::Evaluator`] scores.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: crate::id::RunId,
    pub session_id: crate::id::SessionId,
    pub agent_id: AgentId,
    pub job_id: Option<crate::id::JobId>,
    pub stop: StopReason,
    pub turns: u32,
    pub usage: crate::model::Usage,
    pub duration_ms: u64,
    pub tool_calls: u32,
    pub tool_errors: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ModelId {
        ModelId::new("openai/gpt-4o").unwrap()
    }

    #[test]
    fn an_empty_tool_list_means_every_tool() {
        let agent = AgentSpec::new("coder", model());
        assert!(agent.allows_tool("shell"));
    }

    #[test]
    fn a_scoped_agent_rejects_tools_outside_its_scope() {
        let mut agent = AgentSpec::new("reviewer", model());
        agent.tools = vec!["read_file".into(), "git_diff".into()];
        assert!(agent.allows_tool("read_file"));
        assert!(
            !agent.allows_tool("shell"),
            "a reviewer must not get a shell"
        );
    }

    #[test]
    fn defaults_bound_every_axis_that_can_run_away() {
        let limits = RunLimits::default();
        assert!(limits.max_turns > 0);
        assert!(limits.max_duration_ms > 0);
        assert!(limits.max_total_tokens > 0);
        assert!(limits.max_context_tokens > 0);
        assert!(limits.max_consecutive_tool_errors > 0);
    }

    #[test]
    fn stop_reasons_name_the_limit_that_tripped() {
        let stop = StopReason::LimitReached {
            limit: LimitKind::Turns,
        };
        assert!(!stop.is_success());
        let json = serde_json::to_string(&stop).unwrap();
        assert!(json.contains("turns"), "{json}");
    }
}
