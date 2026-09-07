//! Phase 3 `DoD` 1: the TUI consumes events and imports no runtime type.
//!
//! The property is enforced by the compiler — with no `rivet-runtime` dependency,
//! `use rivet_runtime::…` does not build. So what a test can add is protection against the
//! *edit* that would give the property away: someone adding one line to `Cargo.toml` to
//! reach for one field.

use std::path::Path;

/// The manifest is **parsed**, not searched.
///
/// A string search gets this wrong in both directions: it matches the word inside the
/// comment that explains why the dependency is absent, and it would miss the workspace
/// inheritance spelling `rivet-runtime.workspace = true` if somebody searched for
/// `rivet-runtime = `.
#[test]
fn the_tui_crate_does_not_depend_on_the_runtime() {
    let manifest: toml::Table = toml::from_str(
        &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("this crate's own manifest"),
    )
    .expect("a valid manifest");

    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(table) = manifest.get(section).and_then(toml::Value::as_table) else {
            continue;
        };
        let rivet: Vec<&String> = table.keys().filter(|k| k.starts_with("rivet-")).collect();
        assert!(
            !rivet.contains(&&"rivet-runtime".to_string()),
            "`{section}` names rivet-runtime; the TUI consumes events, it does not reach \
             into what produces them"
        );
        assert!(
            rivet.iter().all(|k| k.as_str() == "rivet-core"),
            "`{section}` names {rivet:?}; the only Rivet crate the TUI may see is \
             rivet-core, which is where the event contract lives"
        );
    }
}

/// The positive half of `DoD` 1: the panels are filled from events and nothing else.
///
/// Five families of hand-made envelopes, folded with no bus, no runtime and no terminal in
/// the process. If any panel needed something an event does not carry, this test could not
/// be written.
#[test]
fn every_panel_is_filled_from_events_alone() {
    use rivet_core::event::{AgentEvent, Event, EventEnvelope, JobEvent, PluginEvent};
    use rivet_core::event::{RuntimeEvent, ToolEvent};
    use rivet_core::id::{AgentId, JobId, RunId, SessionId, ToolCallId};
    use rivet_core::job::JobState;
    use rivet_core::model::ModelId;
    use rivet_tui::AppState;

    let session = SessionId::new();
    let run = RunId::new();
    let call = ToolCallId::new();
    let job = JobId::new();
    let model = ModelId::new("openai/gpt-4o").unwrap();

    let mut state = AppState::default();
    for payload in [
        Event::Runtime(RuntimeEvent::Started {
            version: "0.1.0".into(),
        }),
        Event::Plugin(PluginEvent::Loaded {
            plugin_id: rivet_core::id::PluginId::new("rivet.tool-filesystem").unwrap(),
            capabilities: vec!["tool:read_file".into()],
        }),
        Event::Agent(AgentEvent::RunStarted {
            agent_id: AgentId::new(),
            model: model.clone(),
        }),
        Event::Agent(AgentEvent::TurnStarted { turn: 1 }),
        Event::Agent(AgentEvent::TextDelta {
            text: "reading the readme".into(),
        }),
        Event::Tool(ToolEvent::Requested {
            call_id: call,
            name: "read_file".into(),
        }),
        Event::Tool(ToolEvent::Started {
            call_id: call,
            name: "read_file".into(),
            sandboxed: false,
        }),
        Event::Tool(ToolEvent::Completed {
            call_id: call,
            is_error: false,
            duration_ms: 42,
        }),
        Event::Agent(AgentEvent::RequestCompleted {
            usage: rivet_core::model::Usage {
                input_tokens: 100,
                output_tokens: 20,
                ..rivet_core::model::Usage::default()
            },
            stop_reason: rivet_core::model::StopReason::EndTurn,
            latency_ms: 30,
        }),
        Event::Job(JobEvent::Created {
            job_id: job,
            goal: "implement login".into(),
        }),
        Event::Job(JobEvent::StateChanged {
            job_id: job,
            from: JobState::Pending,
            to: JobState::Running,
            reason: "scheduled".into(),
        }),
        Event::Runtime(RuntimeEvent::SubscriberLagged {
            subscriber: "render.tui".into(),
            dropped: 7,
        }),
    ] {
        state.apply(&EventEnvelope::new(payload).for_run(session, run));
    }

    // Agent panel.
    assert_eq!(state.run.turn, 1);
    assert!(state.run.text.contains("reading the readme"));
    assert_eq!(state.run.tools.len(), 1);
    assert_eq!(state.run.tools[0].name, "read_file");
    assert_eq!(state.run.tools[0].duration_ms, Some(42));

    // Job panel — from `job.*` alone, with no job runtime in the process.
    assert_eq!(state.jobs.jobs.len(), 1);
    assert_eq!(state.jobs.jobs[0].goal, "implement login");
    assert_eq!(state.jobs.jobs[0].state, "Running");

    // Status bar.
    assert_eq!(state.status.model.as_deref(), Some("openai/gpt-4o"));
    assert_eq!(state.status.input_tokens, 100);
    assert_eq!(state.status.output_tokens, 20);
    assert_eq!(state.status.tool_calls, 1);
    assert_eq!(state.status.plugins, 1);
    assert_eq!(state.status.dropped, 7);
    assert_eq!(
        state.status.session.as_deref(),
        Some(session.to_string()).as_deref()
    );
    assert_eq!(
        state.status.run.as_deref(),
        Some(run.to_string()).as_deref()
    );
}
