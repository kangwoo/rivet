//! The screen, as a pure fold over events.
//!
//! [`AppState::apply`] reads no clock, does no I/O and touches no global. That is what
//! lets every panel be tested from a hand-written list of envelopes, with no runtime, no
//! bus and no terminal — the same property `SessionState::replay` has, for the same
//! reason.
//!
//! Everything that grows is bounded. A run streaming for an hour must not turn the UI into
//! a memory leak, so the text, the tool list and the job list are ring buffers that report
//! what they dropped rather than dropping it silently.

use std::collections::BTreeMap;

use rivet_core::event::ToolEvent;
use rivet_core::event::{AgentEvent, Event, EventEnvelope, JobEvent, PluginEvent, RuntimeEvent};

/// How many streamed characters the agent panel keeps.
const TEXT_LIMIT: usize = 8_192;

/// How many tool call lines the agent panel keeps.
const TOOL_LIMIT: usize = 200;

/// Which panel has focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Panel {
    #[default]
    Agent,
    Jobs,
}

/// One tool call, as the events describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolLine {
    pub name: String,
    pub status: ToolStatus,
    pub duration_ms: Option<u64>,
    /// The most recent `tool.execute.progress` message, if there was one.
    pub progress: Option<String>,
}

/// Where a tool call got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Requested,
    Running,
    Done,
    Failed,
    Blocked,
}

impl ToolStatus {
    /// The one-word label the panel prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Requested => "queued",
            Self::Running => "running",
            Self::Done => "ok",
            Self::Failed => "error",
            Self::Blocked => "blocked",
        }
    }
}

/// The agent panel's contents.
#[derive(Clone, Debug, Default)]
pub struct RunView {
    /// Streamed model output, oldest characters dropped first.
    pub text: String,
    /// Characters dropped off the front of `text`.
    pub text_dropped: usize,
    pub turn: u32,
    /// Retries so far, from `agent.request.failed` with `will_retry`.
    pub retries: u32,
    /// The most recent failure that was **not** retried.
    pub last_error: Option<String>,
    /// Tool calls, keyed by call id so a completion can find its start.
    pub tools: Vec<ToolLine>,
    tool_index: BTreeMap<String, usize>,
    /// Tool lines dropped off the front.
    pub tools_dropped: usize,
    /// Why the run stopped, once it has.
    pub stop: Option<String>,
}

/// One job, as `job.*` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobLine {
    pub id: String,
    pub goal: String,
    pub state: String,
    /// Run ids attached to this job, in order.
    pub runs: Vec<String>,
    pub verdict: Option<String>,
}

/// The job panel's contents.
///
/// Empty for the whole of Phase 3: there is no job runtime to publish `job.*` yet. The
/// panel is built and tested anyway, against hand-made envelopes — a panel deferred to
/// Phase 5 is a panel Phase 5 would be tempted to fill by reaching into the job runtime's
/// types instead of its events, which is how the independence rule gets broken later.
#[derive(Clone, Debug, Default)]
pub struct JobView {
    pub jobs: Vec<JobLine>,
}

/// The status bar's contents.
#[derive(Clone, Debug, Default)]
pub struct StatusView {
    pub session: Option<String>,
    pub run: Option<String>,
    pub model: Option<String>,
    pub turn: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tool_calls: u32,
    /// Events the bus reported as lost, summed over the reports **this UI saw**.
    ///
    /// A floor, not a total: a subscriber far enough behind can miss its own lag report.
    /// The help line says so.
    pub dropped: u64,
    /// Plugins loaded, from `plugin.loaded`.
    pub plugins: u32,
    /// Set by `runtime.shutting_down`.
    pub shutting_down: bool,
}

/// Everything on screen. Built only from events.
#[derive(Clone, Debug, Default)]
pub struct AppState {
    pub run: RunView,
    pub jobs: JobView,
    pub status: StatusView,
    pub focus: Panel,
}

impl AppState {
    /// Fold one event in. Pure.
    #[allow(clippy::too_many_lines)] // One arm per topic; splitting it hides the mapping.
    pub fn apply(&mut self, envelope: &EventEnvelope) {
        if let Some(session) = envelope.session_id {
            self.status.session = Some(session.to_string());
        }
        if let Some(run) = envelope.run_id {
            self.status.run = Some(run.to_string());
        }

        match &envelope.payload {
            Event::Agent(agent) => match agent {
                AgentEvent::RunStarted { model, .. } => {
                    self.status.model = Some(model.as_str().to_string());
                    self.run.stop = None;
                }
                // Both ends of a turn report the same number, and the panel shows the
                // turn it is on either way.
                AgentEvent::TurnStarted { turn } | AgentEvent::TurnCompleted { turn } => {
                    self.run.turn = *turn;
                    self.status.turn = *turn;
                }
                AgentEvent::RequestStarted { model, .. } => {
                    self.status.model = Some(model.as_str().to_string());
                }
                AgentEvent::TextDelta { text } => self.push_text(text),
                AgentEvent::RequestCompleted { usage, .. } => {
                    self.status.input_tokens += usage.input_tokens;
                    self.status.output_tokens += usage.output_tokens;
                }
                AgentEvent::RequestFailed {
                    error, will_retry, ..
                } => {
                    if *will_retry {
                        self.run.retries += 1;
                    } else {
                        self.run.last_error = Some(error.clone());
                    }
                }
                AgentEvent::RunCompleted { stop, .. } => {
                    self.run.stop = Some(format!("{stop:?}"));
                }
            },
            Event::Tool(tool) => match tool {
                ToolEvent::Requested { call_id, name } => {
                    self.status.tool_calls += 1;
                    self.push_tool(
                        &call_id.to_string(),
                        ToolLine {
                            name: name.clone(),
                            status: ToolStatus::Requested,
                            duration_ms: None,
                            progress: None,
                        },
                    );
                }
                ToolEvent::Started { call_id, name, .. } => {
                    self.upsert_tool(&call_id.to_string(), name, |line| {
                        line.status = ToolStatus::Running;
                    });
                }
                ToolEvent::Progress { call_id, message } => {
                    self.upsert_tool(&call_id.to_string(), "", |line| {
                        line.progress = Some(message.clone());
                    });
                }
                ToolEvent::Completed {
                    call_id,
                    is_error,
                    duration_ms,
                } => {
                    let (is_error, duration_ms) = (*is_error, *duration_ms);
                    self.upsert_tool(&call_id.to_string(), "", |line| {
                        line.status = if is_error {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Done
                        };
                        line.duration_ms = Some(duration_ms);
                    });
                }
                ToolEvent::Blocked { call_id, reason } => {
                    let reason = reason.clone();
                    self.upsert_tool(&call_id.to_string(), "", |line| {
                        line.status = ToolStatus::Blocked;
                        line.progress = Some(reason.clone());
                    });
                }
                // Phase 4 publishes these. Shown as a progress note rather than dropped,
                // so a policy decision is visible the day it starts being published.
                ToolEvent::PolicyEvaluated {
                    call_id, policy, ..
                } => {
                    let policy = policy.clone();
                    self.upsert_tool(&call_id.to_string(), "", |line| {
                        line.progress = Some(format!("policy: {policy}"));
                    });
                }
                ToolEvent::ApprovalRequested { call_id, reason } => {
                    let reason = reason.clone();
                    self.upsert_tool(&call_id.to_string(), "", |line| {
                        line.progress = Some(format!("awaiting approval: {reason}"));
                    });
                }
                ToolEvent::ApprovalResolved { call_id, approved } => {
                    let approved = *approved;
                    self.upsert_tool(&call_id.to_string(), "", |line| {
                        line.progress =
                            Some(if approved { "approved" } else { "denied" }.to_string());
                    });
                }
            },
            Event::Job(job) => self.apply_job(job),
            Event::Plugin(plugin) => match plugin {
                PluginEvent::Loaded { .. } => self.status.plugins += 1,
                PluginEvent::Unloaded { .. } => {
                    self.status.plugins = self.status.plugins.saturating_sub(1);
                }
                PluginEvent::Discovered { .. } | PluginEvent::LoadFailed { .. } => {}
            },
            Event::Runtime(runtime) => match runtime {
                RuntimeEvent::Started { .. } => {}
                RuntimeEvent::ShuttingDown { .. } => self.status.shutting_down = true,
                RuntimeEvent::SubscriberLagged { dropped, .. } => {
                    self.status.dropped += dropped;
                }
            },
        }
    }

    fn apply_job(&mut self, job: &JobEvent) {
        match job {
            JobEvent::Created { job_id, goal } => {
                let id = job_id.to_string();
                if self.jobs.jobs.iter().all(|j| j.id != id) {
                    self.jobs.jobs.push(JobLine {
                        id,
                        goal: goal.clone(),
                        state: "Pending".to_string(),
                        runs: Vec::new(),
                        verdict: None,
                    });
                }
            }
            JobEvent::StateChanged { job_id, to, .. } => {
                self.upsert_job(&job_id.to_string(), |line| line.state = format!("{to:?}"));
            }
            JobEvent::RunAttached { job_id, run_id, .. } => {
                let run = run_id.to_string();
                self.upsert_job(&job_id.to_string(), |line| line.runs.push(run.clone()));
            }
            JobEvent::ReviewRequested { job_id, .. } => {
                self.upsert_job(&job_id.to_string(), |line| {
                    line.state = "Review".to_string();
                });
            }
            JobEvent::ReviewCompleted { job_id, verdict } => {
                let verdict = format!("{verdict:?}");
                self.upsert_job(&job_id.to_string(), |line| {
                    line.verdict = Some(verdict.clone());
                });
            }
        }
    }

    /// Append streamed text, dropping from the front when it grows past the cap.
    fn push_text(&mut self, text: &str) {
        self.run.text.push_str(text);
        if self.run.text.len() > TEXT_LIMIT {
            let excess = self.run.text.len() - TEXT_LIMIT;
            // Trim to a character boundary: model output is not ASCII.
            let cut = self
                .run
                .text
                .char_indices()
                .map(|(index, _)| index)
                .find(|index| *index >= excess)
                .unwrap_or(self.run.text.len());
            self.run.text.drain(..cut);
            self.run.text_dropped += cut;
        }
    }

    fn push_tool(&mut self, call_id: &str, line: ToolLine) {
        self.run.tools.push(line);
        self.run
            .tool_index
            .insert(call_id.to_string(), self.run.tools.len() - 1);
        if self.run.tools.len() > TOOL_LIMIT {
            self.run.tools.remove(0);
            self.run.tools_dropped += 1;
            // Every index shifts by one; a dropped line's index falls off the front.
            self.run.tool_index.retain(|_, index| {
                if *index == 0 {
                    return false;
                }
                *index -= 1;
                true
            });
        }
    }

    /// Update the line for `call_id`, creating one if the start was never seen.
    ///
    /// Created rather than ignored: the bus is lossy, so a UI that only updated lines it
    /// had seen begin would show *nothing* for a call whose `tool.requested` was dropped —
    /// turning one lost event into a whole missing tool call.
    fn upsert_tool(&mut self, call_id: &str, name: &str, update: impl FnOnce(&mut ToolLine)) {
        if let Some(index) = self.run.tool_index.get(call_id).copied()
            && let Some(line) = self.run.tools.get_mut(index)
        {
            update(line);
            return;
        }
        let mut line = ToolLine {
            name: if name.is_empty() { "?" } else { name }.to_string(),
            status: ToolStatus::Running,
            duration_ms: None,
            progress: None,
        };
        update(&mut line);
        self.push_tool(call_id, line);
    }

    fn upsert_job(&mut self, job_id: &str, update: impl FnOnce(&mut JobLine)) {
        if let Some(line) = self.jobs.jobs.iter_mut().find(|j| j.id == job_id) {
            update(line);
            return;
        }
        let mut line = JobLine {
            id: job_id.to_string(),
            goal: String::new(),
            state: "Pending".to_string(),
            runs: Vec::new(),
            verdict: None,
        };
        update(&mut line);
        self.jobs.jobs.push(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::id::ToolCallId;

    fn envelope(payload: Event) -> EventEnvelope {
        EventEnvelope::new(payload)
    }

    #[test]
    fn streamed_text_is_capped_and_says_what_it_dropped() {
        let mut state = AppState::default();
        for _ in 0..200 {
            state.apply(&envelope(Event::Agent(AgentEvent::TextDelta {
                text: "x".repeat(100),
            })));
        }
        assert!(state.run.text.len() <= TEXT_LIMIT);
        assert!(state.run.text_dropped > 0, "a silent drop is a lie");
    }

    #[test]
    fn a_completion_without_a_start_still_shows_a_tool_line() {
        // The bus is lossy. One dropped `tool.requested` must not erase the whole call.
        let mut state = AppState::default();
        let call = ToolCallId::new();
        state.apply(&envelope(Event::Tool(ToolEvent::Completed {
            call_id: call,
            is_error: false,
            duration_ms: 12,
        })));
        assert_eq!(state.run.tools.len(), 1);
        assert_eq!(state.run.tools[0].status, ToolStatus::Done);
    }

    #[test]
    fn the_tool_list_is_bounded() {
        let mut state = AppState::default();
        for _ in 0..(TOOL_LIMIT + 20) {
            state.apply(&envelope(Event::Tool(ToolEvent::Requested {
                call_id: ToolCallId::new(),
                name: "t".into(),
            })));
        }
        assert_eq!(state.run.tools.len(), TOOL_LIMIT);
        assert_eq!(state.run.tools_dropped, 20);
    }

    #[test]
    fn a_dropped_tool_line_does_not_leave_a_stale_index() {
        // The index maps call id -> position, and every eviction shifts every position.
        // Getting that wrong writes a completion into another call's line.
        let mut state = AppState::default();
        let mut ids = Vec::new();
        for _ in 0..(TOOL_LIMIT + 5) {
            let call = ToolCallId::new();
            ids.push(call);
            state.apply(&envelope(Event::Tool(ToolEvent::Requested {
                call_id: call,
                name: "t".into(),
            })));
        }
        let last = *ids.last().unwrap();
        state.apply(&envelope(Event::Tool(ToolEvent::Completed {
            call_id: last,
            is_error: false,
            duration_ms: 7,
        })));
        assert_eq!(
            state.run.tools.last().unwrap().duration_ms,
            Some(7),
            "the completion landed on the wrong line"
        );
        assert_eq!(state.run.tools.len(), TOOL_LIMIT);
    }
}
