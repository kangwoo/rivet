//! `--jsonl`: one bus event per line.
//!
//! **This is observation, not a session export.** The bus is lossy on purpose — a slow
//! consumer loses events and is told so — so a stream of it can never be a faithful record
//! of what happened. `rivet session show --json` reads the durable log, which is the thing
//! that can be replayed.

use std::io::Write;

use async_trait::async_trait;
use rivet_core::event::{EventEnvelope, EventSubscriber};

/// Writes each event to stdout as one JSON object per line.
#[derive(Debug, Default)]
pub struct JsonlRenderer;

impl JsonlRenderer {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl EventSubscriber for JsonlRenderer {
    fn name(&self) -> &'static str {
        "render.jsonl"
    }

    async fn on_event(&self, envelope: &EventEnvelope) {
        if let Ok(line) = serde_json::to_string(envelope) {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}
