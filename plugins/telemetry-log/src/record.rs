//! One bus envelope → one `tracing` event, with a fixed field set.
//!
//! **The payload is not poured in as JSON.** That would make this a worse `--jsonl`: the
//! whole value of a structured log is that the fields are the *same* on every record, so
//! `topic` and `run` can be indexed and grouped. A blob under one key is a string a human
//! reads and a machine cannot filter. So five correlation fields are always present, and
//! each family contributes a small, named handful.
//!
//! `tracing` needs its field names as literals at the macro call site, which is why this is
//! a `match` over families rather than a loop over a map. That is a feature here: adding a
//! field is a visible edit at the line that decides it.

use rivet_core::error::Error;
use rivet_core::event::ToolEvent;
use rivet_core::event::{AgentEvent, Event, EventEnvelope, JobEvent, PluginEvent, RuntimeEvent};

/// The `tracing` level this plugin emits at.
///
/// A copy of the four levels the config accepts, rather than `tracing::Level`, so the
/// vocabulary a `rivet.toml` may use is decided here and not by a dependency's `FromStr`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
}

impl Level {
    /// # Errors
    /// Anything but the four names.
    pub fn parse(name: &str) -> rivet_core::Result<Self> {
        match name {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            other => Err(Error::invalid_argument(format!(
                "unknown level `{other}`; expected one of trace, debug, info, warn"
            ))),
        }
    }
}

/// Emit one record for `envelope`.
///
/// The five correlation fields are on every record. `run` and `session` are empty strings
/// rather than absent for a runtime-level event, because a log consumer that has to handle
/// a missing key differently from an empty one is a log consumer with two code paths.
pub fn emit(level: Level, envelope: &EventEnvelope) {
    let topic = envelope.topic();
    let event_id = envelope.id.to_string();
    let run = envelope.run_id.map(|id| id.to_string()).unwrap_or_default();
    let session = envelope
        .session_id
        .map(|id| id.to_string())
        .unwrap_or_default();
    let at = envelope.at.to_string();
    let detail = detail_of(&envelope.payload);

    macro_rules! log_at {
        ($level:ident) => {
            tracing::$level!(
                topic,
                event_id,
                session,
                run,
                at,
                subject = detail.subject.as_deref().unwrap_or(""),
                count = detail.count.unwrap_or_default(),
                error = detail.error,
                "rivet event"
            )
        };
    }

    match level {
        Level::Trace => log_at!(trace),
        Level::Debug => log_at!(debug),
        Level::Info => log_at!(info),
        Level::Warn => log_at!(warn),
    }
}

/// The few fields worth pulling out of a payload, in a shape every family shares.
///
/// Three, not one per payload field. A record with a different field set per topic cannot
/// be queried across topics, which is the only thing a structured log is better at than
/// the JSONL stream.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Detail {
    /// What the event is about: a tool name, a plugin id, a model id, a subscriber name.
    pub subject: Option<String>,
    /// The event's one number: duration, turn, attempt, dropped count.
    pub count: Option<u64>,
    /// Whether this record reports something going wrong.
    pub error: bool,
}

/// Pull `subject`, `count` and `error` out of a payload.
///
/// No wildcard in either layer, for the reason `Event::one_of_each`'s doc gives: a new
/// family or variant should stop compiling here rather than log a record with three empty
/// fields.
#[must_use]
pub fn detail_of(event: &Event) -> Detail {
    match event {
        Event::Agent(agent) => agent_detail(agent),
        Event::Tool(tool) => tool_detail(tool),
        Event::Job(job) => job_detail(job),
        Event::Plugin(plugin) => plugin_detail(plugin),
        Event::Runtime(runtime) => runtime_detail(runtime),
    }
}

fn agent_detail(agent: &AgentEvent) -> Detail {
    match agent {
        AgentEvent::RunStarted { model, .. } => Detail {
            subject: Some(model.as_str().to_string()),
            ..Detail::default()
        },
        AgentEvent::TurnStarted { turn } | AgentEvent::TurnCompleted { turn } => Detail {
            count: Some(u64::from(*turn)),
            ..Detail::default()
        },
        AgentEvent::RequestStarted {
            model,
            input_tokens_estimate,
        } => Detail {
            subject: Some(model.as_str().to_string()),
            count: Some(*input_tokens_estimate),
            error: false,
        },
        // Length only. The text itself is the thing `include_conversation` gates, and
        // a record that carried it would put the conversation in the log by the back
        // door -- through a subscription the operator did ask for.
        AgentEvent::TextDelta { text } => Detail {
            count: Some(text.len() as u64),
            ..Detail::default()
        },
        AgentEvent::RequestCompleted {
            usage, latency_ms, ..
        } => Detail {
            subject: Some(format!(
                "{} in / {} out",
                usage.input_tokens, usage.output_tokens
            )),
            count: Some(*latency_ms),
            error: false,
        },
        AgentEvent::RequestFailed { error, attempt, .. } => Detail {
            subject: Some(error.clone()),
            count: Some(u64::from(*attempt)),
            error: true,
        },
        AgentEvent::RunCompleted { turns, stop } => Detail {
            subject: Some(format!("{stop:?}")),
            count: Some(u64::from(*turns)),
            error: !stop.is_success(),
        },
    }
}

fn tool_detail(tool: &ToolEvent) -> Detail {
    match tool {
        ToolEvent::Requested { name, .. } | ToolEvent::Started { name, .. } => Detail {
            subject: Some(name.clone()),
            ..Detail::default()
        },
        ToolEvent::PolicyEvaluated { policy, .. } => Detail {
            subject: Some(policy.clone()),
            ..Detail::default()
        },
        ToolEvent::ApprovalRequested { reason, .. } => Detail {
            subject: Some(reason.clone()),
            ..Detail::default()
        },
        ToolEvent::ApprovalResolved { approved, .. } => Detail {
            subject: Some(if *approved { "approved" } else { "denied" }.to_string()),
            count: None,
            error: !*approved,
        },
        ToolEvent::Progress { message, .. } => Detail {
            subject: Some(message.clone()),
            ..Detail::default()
        },
        ToolEvent::Completed {
            is_error,
            duration_ms,
            ..
        } => Detail {
            subject: None,
            count: Some(*duration_ms),
            error: *is_error,
        },
        ToolEvent::Blocked { reason, .. } => Detail {
            subject: Some(reason.clone()),
            count: None,
            error: true,
        },
    }
}

fn job_detail(job: &JobEvent) -> Detail {
    match job {
        JobEvent::Created { job_id, .. } | JobEvent::ReviewRequested { job_id, .. } => Detail {
            subject: Some(job_id.to_string()),
            ..Detail::default()
        },
        JobEvent::StateChanged { job_id, to, .. } => Detail {
            subject: Some(format!("{job_id} -> {to:?}")),
            ..Detail::default()
        },
        JobEvent::RunAttached {
            job_id, attempt, ..
        } => Detail {
            subject: Some(job_id.to_string()),
            count: Some(u64::from(*attempt)),
            error: false,
        },
        JobEvent::ReviewCompleted { job_id, verdict } => Detail {
            subject: Some(format!("{job_id} {verdict:?}")),
            ..Detail::default()
        },
    }
}

fn plugin_detail(plugin: &PluginEvent) -> Detail {
    match plugin {
        PluginEvent::Discovered { plugin_id } | PluginEvent::Unloaded { plugin_id } => Detail {
            subject: Some(plugin_id.as_str().to_string()),
            ..Detail::default()
        },
        PluginEvent::Loaded {
            plugin_id,
            capabilities,
        } => Detail {
            subject: Some(plugin_id.as_str().to_string()),
            count: Some(capabilities.len() as u64),
            error: false,
        },
        PluginEvent::LoadFailed { plugin_id, error } => Detail {
            subject: Some(format!("{plugin_id}: {error}")),
            count: None,
            error: true,
        },
    }
}

fn runtime_detail(runtime: &RuntimeEvent) -> Detail {
    match runtime {
        RuntimeEvent::Started { version } => Detail {
            subject: Some(version.clone()),
            ..Detail::default()
        },
        RuntimeEvent::ShuttingDown { reason } => Detail {
            subject: Some(reason.clone()),
            ..Detail::default()
        },
        // The one record that reports the log's own incompleteness.
        RuntimeEvent::SubscriberLagged {
            subscriber,
            dropped,
        } => Detail {
            subject: Some(subscriber.clone()),
            count: Some(*dropped),
            error: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sample_produces_a_record_with_a_topic() {
        // The negative claim that matters: no variant falls through to an empty detail
        // because somebody added it and forgot this file. The `match` has no wildcard, so
        // "forgot" is a compile error -- this checks the arms are not stubs.
        for event in Event::one_of_each() {
            let detail = detail_of(&event);
            assert!(
                detail.subject.is_some() || detail.count.is_some() || detail.error,
                "`{}` produces an empty record",
                event.topic()
            );
        }
    }

    #[test]
    fn a_text_delta_records_its_length_and_not_its_text() {
        let detail = detail_of(&Event::Agent(AgentEvent::TextDelta {
            text: "a secret".to_string(),
        }));
        assert_eq!(detail.count, Some(8));
        assert_eq!(detail.subject, None, "the text itself must not reach a log");
    }

    #[test]
    fn the_records_that_report_trouble_are_marked() {
        let blocked = detail_of(&Event::Tool(ToolEvent::Blocked {
            call_id: rivet_core::id::ToolCallId::new(),
            reason: "outside the agent's scope".into(),
        }));
        assert!(blocked.error);

        let lagged = detail_of(&Event::Runtime(RuntimeEvent::SubscriberLagged {
            subscriber: "telemetry.log".into(),
            dropped: 12,
        }));
        assert!(lagged.error, "the log has to be able to say it lost events");
        assert_eq!(lagged.count, Some(12));
    }

    #[test]
    fn levels_are_the_four_the_config_documents() {
        for name in ["trace", "debug", "info", "warn"] {
            Level::parse(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        assert!(Level::parse("error").is_err(), "not in the vocabulary");
    }
}
