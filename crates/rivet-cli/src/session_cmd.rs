//! `rivet session list | show | fork`.

use std::sync::Arc;

use rivet_core::error::Error;
use rivet_core::id::SessionId;
use rivet_core::session::{SessionState, SessionStore};
use rivet_session::JsonlSessionStore;

use crate::config::Config;

/// How many sessions `list` shows by default.
const LIST_LIMIT: usize = 20;

/// # Errors
/// Storage failures.
pub async fn list(config: &Config) -> rivet_core::Result<()> {
    let store = JsonlSessionStore::new(&config.sessions_dir);
    let sessions = store.list(LIST_LIMIT).await?;
    if sessions.is_empty() {
        println!("no sessions in {}", config.sessions_dir.display());
        return Ok(());
    }
    for summary in sessions {
        println!(
            "{}  {}  {:>4} events  {}{}",
            summary.id,
            summary.updated_at.to_rfc3339(),
            summary.last_seq,
            summary.title,
            if summary.closed { "  (closed)" } else { "" }
        );
    }
    Ok(())
}

/// Print a session's durable log.
///
/// This, not `--jsonl`, is the way to reconstruct a session: the log is the record, and
/// the bus is lossy.
///
/// # Errors
/// A malformed id, or storage failures.
pub async fn show(config: &Config, id: &str, as_json: bool) -> rivet_core::Result<()> {
    let session_id = parse(id)?;
    let store: Arc<dyn SessionStore> = Arc::new(JsonlSessionStore::new(&config.sessions_dir));
    let events = rivet_runtime::session_recovery::read_all(store.as_ref(), session_id).await?;

    if as_json {
        for event in &events {
            println!("{}", serde_json::to_string(event)?);
        }
        return Ok(());
    }

    for event in &events {
        let name = serde_json::to_value(&event.event)?["type"]
            .as_str()
            .unwrap_or("?")
            .to_string();
        println!("{:>4}  {}  {name}", event.seq, event.at.to_rfc3339());
    }

    let state = SessionState::replay(&events);
    println!(
        "\n{} message(s), {} tokens in / {} out{}",
        state.messages.len(),
        state.total_usage.input_tokens,
        state.total_usage.output_tokens,
        if state.closed { ", closed" } else { "" }
    );
    Ok(())
}

/// Branch a session at an event boundary.
///
/// This is the recovery path for a log that cannot be repaired by appending: fork before
/// the problem and carry on from there.
///
/// # Errors
/// A malformed id, an out-of-range sequence number, or storage failures.
pub async fn fork(config: &Config, id: &str, at_seq: u64) -> rivet_core::Result<()> {
    let source = parse(id)?;
    let store = JsonlSessionStore::new(&config.sessions_dir);
    let new_id = SessionId::new();
    let summary = store.fork(source, at_seq, new_id).await?;
    println!("{} (forked from {source} at seq {at_seq})", summary.id);
    Ok(())
}

fn parse(id: &str) -> rivet_core::Result<SessionId> {
    id.parse()
        .map_err(|e| Error::invalid_argument(format!("`{id}` is not a session id")).with_cause(e))
}
