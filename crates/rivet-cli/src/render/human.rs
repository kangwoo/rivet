//! The default renderer: streamed text, and a short line per tool call.

use std::io::Write;
use std::sync::Mutex;

use async_trait::async_trait;
use rivet_core::event::{AgentEvent, Event, EventEnvelope, EventSubscriber, ToolEvent};

/// Streams the answer to stdout and summarizes everything else on stderr.
///
/// The split matters for shell use: `rivet "..." > answer.md` should capture the answer,
/// not the progress report.
#[derive(Debug, Default)]
pub struct HumanRenderer {
    /// Whether a text delta has been written since the last non-text event.
    streaming: Mutex<bool>,
}

impl HumanRenderer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn end_stream(&self) {
        let mut streaming = self
            .streaming
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *streaming {
            println!();
            *streaming = false;
        }
    }
}

#[async_trait]
impl EventSubscriber for HumanRenderer {
    fn name(&self) -> &'static str {
        "render.human"
    }

    async fn on_event(&self, envelope: &EventEnvelope) {
        match &envelope.payload {
            Event::Agent(AgentEvent::TextDelta { text }) => {
                let mut streaming = self
                    .streaming
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *streaming = true;
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            Event::Agent(AgentEvent::RequestFailed {
                error,
                will_retry,
                attempt,
            }) => {
                self.end_stream();
                if *will_retry {
                    eprintln!("  ! attempt {attempt} failed, retrying: {error}");
                } else {
                    eprintln!("  ! {error}");
                }
            }
            Event::Agent(AgentEvent::RunCompleted { turns, stop }) => {
                self.end_stream();
                eprintln!("  · {turns} turn(s), stopped: {}", describe(stop));
            }
            Event::Tool(ToolEvent::Started { name, .. }) => {
                self.end_stream();
                eprintln!("  → {name}");
            }
            Event::Tool(ToolEvent::Completed {
                is_error,
                duration_ms,
                ..
            }) => {
                eprintln!("  {} {duration_ms}ms", if *is_error { "✗" } else { "✓" });
            }
            Event::Tool(ToolEvent::Blocked { reason, .. }) => {
                self.end_stream();
                eprintln!("  ⊘ blocked: {reason}");
            }
            Event::Tool(ToolEvent::Progress { message, .. }) => eprintln!("    {message}"),
            _ => {}
        }
    }
}

fn describe(stop: &rivet_core::agent::StopReason) -> String {
    use rivet_core::agent::StopReason;
    match stop {
        StopReason::EndTurn => "done".to_string(),
        StopReason::LimitReached { limit } => format!("{limit:?} limit reached"),
        StopReason::Cancelled => "cancelled".to_string(),
        StopReason::PolicyBlocked { reason } => format!("blocked by policy: {reason}"),
        StopReason::Error { message } => format!("error: {message}"),
    }
}
