//! Shared fixtures for the session-store tests.

// Each test binary uses a different subset.
#![allow(dead_code)]

use rivet_core::id::{RunId, SessionId, ToolCallId};
use rivet_core::model::{ContentBlock, Message, ModelId, Role, StopReason, Usage};
use rivet_core::session::SessionEvent;
use rivet_core::tool::{ToolCall, ToolResult};
use rivet_session::JsonlSessionStore;

/// A store rooted in a fresh temporary directory.
pub fn store() -> (JsonlSessionStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = JsonlSessionStore::new(dir.path().join("sessions"));
    (store, dir)
}

pub fn created() -> SessionEvent {
    SessionEvent::Created {
        workspace_root: "/repo".into(),
        parent: None,
    }
}

pub fn user(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        message: Message::user(text),
    }
}

pub fn assistant_calling(run_id: RunId, call: &ToolCall) -> SessionEvent {
    SessionEvent::AssistantMessage {
        run_id,
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(call.clone())],
        },
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

pub fn tool_call(name: &str) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        name: name.into(),
        input: serde_json::json!({}),
    }
}

pub fn completed(run_id: RunId, call: &ToolCall, content: &str) -> SessionEvent {
    SessionEvent::ToolCompleted {
        run_id,
        call_id: call.id,
        result: ToolResult::ok(content),
        duration_ms: 3,
    }
}

pub fn run_started(run_id: RunId) -> SessionEvent {
    SessionEvent::RunStarted {
        run_id,
        agent_id: rivet_core::id::AgentId::new(),
        model: ModelId::new("openai/gpt-4o").unwrap(),
        task_id: None,
    }
}

/// The raw bytes of a session's log.
pub fn log_bytes(store: &JsonlSessionStore, id: SessionId) -> Vec<u8> {
    std::fs::read(rivet_session::layout::log_path(store.root(), id)).expect("log")
}

/// Overwrite a session's log with raw bytes, simulating a crash.
pub fn write_log_bytes(store: &JsonlSessionStore, id: SessionId, bytes: &[u8]) {
    std::fs::write(rivet_session::layout::log_path(store.root(), id), bytes).expect("write log");
}
