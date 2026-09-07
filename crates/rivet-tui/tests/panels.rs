//! The three regions, drawn.
//!
//! Every test here builds its state from envelopes and renders through ratatui's
//! `TestBackend`, so what is asserted is what a terminal would print.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rivet_core::event::{AgentEvent, Event, EventEnvelope, JobEvent, RuntimeEvent, ToolEvent};
use rivet_core::id::{AgentId, JobId, RunId, SessionId, ToolCallId};
use rivet_core::job::{JobState, ReviewVerdict};
use rivet_core::model::ModelId;
use rivet_tui::{AppState, NO_JOBS, draw, status_line};

/// Fold a list of payloads, correlated to one run.
fn fold(payloads: Vec<Event>) -> AppState {
    let session = SessionId::new();
    let run = RunId::new();
    let mut state = AppState::default();
    for payload in payloads {
        state.apply(&EventEnvelope::new(payload).for_run(session, run));
    }
    state
}

/// Everything the terminal would show, as one string.
fn render(state: &AppState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    terminal
        .draw(|frame| draw(frame, state))
        .expect("draw succeeds");
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(width as usize)
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_agent_panel_follows_a_run_from_start_to_stop() {
    let call = ToolCallId::new();
    let state = fold(vec![
        Event::Agent(AgentEvent::RunStarted {
            agent_id: AgentId::new(),
            model: ModelId::new("openai/gpt-4o").unwrap(),
        }),
        Event::Agent(AgentEvent::TurnStarted { turn: 1 }),
        Event::Agent(AgentEvent::TextDelta {
            text: "looking".into(),
        }),
        Event::Agent(AgentEvent::RequestFailed {
            error: "429".into(),
            will_retry: true,
            attempt: 1,
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
        Event::Tool(ToolEvent::Progress {
            call_id: call,
            message: "halfway".into(),
        }),
        Event::Tool(ToolEvent::Completed {
            call_id: call,
            is_error: false,
            duration_ms: 42,
        }),
        Event::Agent(AgentEvent::RunCompleted {
            turns: 1,
            stop: rivet_core::agent::StopReason::EndTurn,
        }),
    ]);

    let screen = render(&state, 100, 20);
    assert!(screen.contains("looking"), "{screen}");
    assert!(screen.contains("read_file"), "{screen}");
    assert!(screen.contains("42ms"), "{screen}");
    assert!(screen.contains("halfway"), "{screen}");
    assert!(screen.contains("retried 1"), "{screen}");
    assert!(screen.contains("stopped"), "{screen}");
}

#[test]
fn a_blocked_tool_call_says_why() {
    let call = ToolCallId::new();
    let state = fold(vec![
        Event::Tool(ToolEvent::Requested {
            call_id: call,
            name: "write_file".into(),
        }),
        Event::Tool(ToolEvent::Blocked {
            call_id: call,
            reason: "outside the agent's scope".into(),
        }),
    ]);
    let screen = render(&state, 100, 20);
    assert!(screen.contains("blocked"), "{screen}");
    assert!(screen.contains("outside the agent"), "{screen}");
}

#[test]
fn the_job_panel_renders_what_job_events_carry() {
    // No job runtime exists. The panel is built from `job.*` alone, which is the property
    // that keeps Phase 5 from filling it by importing `rivet-job`.
    let job = JobId::new();
    let run = RunId::new();
    let state = fold(vec![
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
        Event::Job(JobEvent::RunAttached {
            job_id: job,
            run_id: run,
            attempt: 1,
        }),
        Event::Job(JobEvent::ReviewRequested {
            job_id: job,
            reviewer: AgentId::new(),
        }),
        Event::Job(JobEvent::ReviewCompleted {
            job_id: job,
            verdict: ReviewVerdict::RequestChanges,
        }),
    ]);

    let screen = render(&state, 100, 20);
    assert!(screen.contains("implement login"), "{screen}");
    assert!(screen.contains("Review"), "{screen}");
    assert!(screen.contains("RequestChanges"), "{screen}");
}

#[test]
fn the_job_panel_says_where_jobs_come_from_when_empty() {
    // An empty panel that says nothing reads as broken. This one says which phase.
    let screen = render(&AppState::default(), 100, 20);
    // The panel wraps, so the sentence can arrive on two lines. Both halves have to be
    // there; what is being checked is that the panel explains itself rather than sitting
    // blank.
    assert!(
        screen.contains("no jobs"),
        "expected `{NO_JOBS}` in:\n{screen}"
    );
    assert!(
        screen.contains("Phase 5"),
        "expected `{NO_JOBS}` in:\n{screen}"
    );
}

#[test]
fn the_status_bar_counts_drops_from_the_lag_report() {
    // The self-consistent bit of the design: the bus reports what it lost, so the UI can
    // show it. And the sum is a floor, which the `≥` says out loud.
    let state = fold(vec![
        Event::Runtime(RuntimeEvent::SubscriberLagged {
            subscriber: "render.tui".into(),
            dropped: 12,
        }),
        Event::Runtime(RuntimeEvent::SubscriberLagged {
            subscriber: "telemetry.log".into(),
            dropped: 30,
        }),
    ]);
    assert_eq!(state.status.dropped, 42);
    assert!(
        status_line(&state, 200).contains("≥42 dropped"),
        "{}",
        status_line(&state, 200)
    );
}

#[test]
fn the_status_bar_shows_only_what_events_carry() {
    // The negative assertion that keeps the crate's rule honest. The profile name and the
    // workspace root are things an operator would like on the status bar and no event
    // carries -- so they are not there, and the fix would be a new event, not an import.
    let state = fold(vec![Event::Runtime(RuntimeEvent::Started {
        version: "0.1.0".into(),
    })]);
    let line = status_line(&state, 200);
    for absent in ["developer", "readonly", "workspace", "profile"] {
        assert!(
            !line.contains(absent),
            "`{absent}` is on the status bar, and no event carries it: {line}"
        );
    }
}

#[test]
fn a_shutting_down_runtime_shows_on_the_status_bar() {
    let state = fold(vec![Event::Runtime(RuntimeEvent::ShuttingDown {
        reason: "run finished".into(),
    })]);
    assert!(status_line(&state, 200).contains("shutting down"));
}

#[test]
fn the_three_panels_fit_an_eighty_column_terminal() {
    // 80×24 is the floor a terminal UI has to work at. What this catches is a layout that
    // only adds up on a wide screen: panels overlapping, or the status bar pushed off.
    let call = ToolCallId::new();
    let state = fold(vec![
        Event::Agent(AgentEvent::RunStarted {
            agent_id: AgentId::new(),
            model: ModelId::new("openai/gpt-4o").unwrap(),
        }),
        Event::Agent(AgentEvent::TurnStarted { turn: 2 }),
        Event::Tool(ToolEvent::Requested {
            call_id: call,
            name: "search".into(),
        }),
    ]);

    let screen = render(&state, 80, 24);
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(rows.len(), 24);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.chars().count(), 80, "row {index} is not 80 columns");
    }
    assert!(rows[0].contains("Jobs"), "{screen}");
    assert!(rows[0].contains("Agent"), "{screen}");
    // The status bar is the last row, and it survives the narrow width.
    assert!(rows[23].contains("turn 2"), "status bar: {:?}", rows[23]);
    assert!(rows[23].contains("[q] quit"), "status bar: {:?}", rows[23]);
}

#[test]
fn an_elided_ring_buffer_says_how_much_it_dropped() {
    let mut state = AppState::default();
    for _ in 0..300 {
        state.apply(&EventEnvelope::new(Event::Agent(AgentEvent::TextDelta {
            text: "x".repeat(100),
        })));
    }
    let screen = render(&state, 100, 30);
    assert!(screen.contains("elided"), "{screen}");
}
