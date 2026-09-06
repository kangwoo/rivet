//! Starting and resuming runs.

use std::path::Path;
use std::sync::Arc;

use rivet_core::agent::{AgentSpec, RunSummary, StopReason};
use rivet_core::error::Error;
use rivet_core::event::EventSubscriber;
use rivet_core::id::SessionId;
use rivet_core::model::Message;
use rivet_core::retry::ExponentialBackoff;
use rivet_core::session::{SessionEvent, SessionState, SessionStore};
use rivet_runtime::agent_loop::{AgentLoop, RunConfig};
use rivet_runtime::context::ContextAssembler;
use rivet_runtime::jitter::FullJitter;
use rivet_session::JsonlSessionStore;
use tokio_util::sync::CancellationToken;

use crate::bootstrap::{self, Loaded};
use crate::config::Config;
use crate::render::{human::HumanRenderer, jsonl::JsonlRenderer};

/// How output is presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Human,
    Jsonl,
}

/// Start a new session and run one prompt.
///
/// # Errors
/// Configuration problems, plugin load failures, and session-store failures.
pub async fn start(
    config: &Config,
    prompt: &str,
    output: Output,
) -> rivet_core::Result<RunSummary> {
    // Before a session exists: an empty session left behind by a missing key is noise in
    // `rivet session list` forever.
    config.check_credentials()?;
    let loaded = bootstrap::load(config).await?;
    report_deferred(&loaded);
    let store = Arc::new(JsonlSessionStore::new(&config.sessions_dir));

    let session_id = SessionId::new();
    store
        .create(
            session_id,
            SessionEvent::Created {
                workspace_root: config.workspace.root().display().to_string(),
                parent: None,
            },
        )
        .await?;
    eprintln!("session {session_id}");

    let state = SessionState::replay(&store.read(session_id, 1, 1_000).await?);
    drive(
        config,
        &loaded,
        store,
        session_id,
        state,
        Some(Message::user(prompt)),
        output,
    )
    .await
}

/// Continue an interrupted session.
///
/// # Errors
/// As [`start`], plus [`rivet_core::error::ErrorKind::Storage`] when the log cannot be
/// repaired by appending, and `InvalidArgument` when the session was created against a
/// different workspace.
pub async fn resume(
    config: &Config,
    session: &str,
    output: Output,
) -> rivet_core::Result<Option<RunSummary>> {
    config.check_credentials()?;
    let session_id: SessionId = session.parse().map_err(|e| {
        Error::invalid_argument(format!("`{session}` is not a session id")).with_cause(e)
    })?;

    let store = Arc::new(JsonlSessionStore::new(&config.sessions_dir));
    let events = rivet_runtime::session_recovery::read_all(store.as_ref(), session_id).await?;
    check_workspace(config, &events)?;

    // Close whatever the interruption left open, before anything else looks at the
    // conversation. Doing this in the runtime rather than here means every host that
    // resumes a session gets the same repair.
    let state =
        rivet_runtime::session_recovery::close_interrupted(store.as_ref(), session_id).await?;

    match rivet_runtime::session_recovery::resume_plan(&state) {
        rivet_runtime::session_recovery::ResumePlan::Refuse(reason) => {
            eprintln!("{reason}");
            Ok(None)
        }
        rivet_runtime::session_recovery::ResumePlan::Continue => {
            let loaded = bootstrap::load(config).await?;
            report_deferred(&loaded);
            let summary = drive(config, &loaded, store, session_id, state, None, output).await?;
            Ok(Some(summary))
        }
    }
}

/// Refuse to replay a session that was recorded somewhere else.
///
/// `session.created` records the workspace root precisely so this can be checked. Silently
/// continuing would run another repository's tool calls against this one.
fn check_workspace(
    config: &Config,
    events: &[rivet_core::session::StoredEvent],
) -> rivet_core::Result<()> {
    let Some(recorded) = events.iter().find_map(|e| match &e.event {
        SessionEvent::Created { workspace_root, .. } => Some(workspace_root.clone()),
        _ => None,
    }) else {
        return Ok(());
    };
    let current = config.workspace.root();
    if Path::new(&recorded) == current {
        return Ok(());
    }
    Err(Error::invalid_argument(format!(
        "this session was created in `{recorded}` but the workspace here is `{}`. \
         Resuming would run its tool calls against a different tree; \
         run `rivet resume` from the original workspace instead.",
        current.display()
    )))
}

/// Wire up the renderers, the signal handler and the loop, then run.
async fn drive(
    config: &Config,
    loaded: &Loaded,
    store: Arc<JsonlSessionStore>,
    session_id: SessionId,
    state: SessionState,
    input: Option<Message>,
    output: Output,
) -> rivet_core::Result<RunSummary> {
    let subscriber: Arc<dyn EventSubscriber> = match output {
        Output::Human => Arc::new(HumanRenderer::new()),
        Output::Jsonl => Arc::new(JsonlRenderer::new()),
    };
    let render_task = loaded.bus.attach(subscriber);

    let agent = agent_spec(config);
    let providers = loaded.registry.context_providers().await;
    // Phase 1's agent never names providers, so this takes the "all of them" branch. The
    // selecting branch exists so a named agent works the moment there is a way to pick one.
    let assembler = ContextAssembler::for_agent(providers, &agent.context_providers)?;
    let cancel = CancellationToken::new();
    let signals = crate::signals::install(cancel.clone());

    let mut cfg = RunConfig::new(agent, session_id, config.workspace.clone());
    cfg.profile = config.profile.name().to_string();
    cfg.unattended = config.unattended;
    cfg.permissions = config.profile.permissions();
    cfg.cancel = cancel;

    let agent_loop = AgentLoop::new(
        loaded.registry.clone(),
        store,
        loaded.registry.events(),
        assembler,
        Arc::new(ExponentialBackoff::default()),
        Arc::new(FullJitter::for_run(cfg.run_id)),
    );

    let summary = agent_loop.run(cfg, state, input).await;

    signals.abort();
    loaded.shutdown();
    // Give the renderer a moment to drain before the process exits.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    render_task.abort();

    let summary = summary?;
    if output == Output::Human {
        report(&summary);
    }
    Ok(summary)
}

/// The agent this configuration describes.
fn agent_spec(config: &Config) -> AgentSpec {
    let mut agent = AgentSpec::new("rivet", config.model.clone());
    agent.instructions.clone_from(&config.instructions);
    agent.limits = config.limits;
    if let Some(scope) = config.profile.tool_scope() {
        agent.tools = scope;
    }
    agent
}

fn report(summary: &RunSummary) {
    eprintln!(
        "  · {} in / {} out tokens, {} tool call(s), {}ms",
        summary.usage.input_tokens,
        summary.usage.output_tokens,
        summary.tool_calls,
        summary.duration_ms
    );
    if let StopReason::Error { message } = &summary.stop {
        eprintln!("  ! {message}");
    }
}

fn report_deferred(loaded: &Loaded) {
    for id in &loaded.deferred {
        eprintln!("  · `{id}` is enabled but ships in a later phase; skipping");
    }
}
