//! The telemetry plugin, driven the way a host drives it.
//!
//! The unit tests in `src/` cover the settings and the record shape. These load the plugin
//! through the real `PluginLoader` against real profile grants, because the property that
//! matters — "an unconfigured telemetry plugin loads under every shipped profile" — is a
//! statement about the meet, and the meet only happens in the loader.

use std::sync::Arc;
use std::sync::{Mutex, PoisonError};

use rivet_core::capability::{FsScope, Permission, PermissionSet, TopicScope};
use rivet_core::event::{AgentEvent, Event, EventBus, EventEnvelope, ToolEvent};
use rivet_core::id::{PluginId, ToolCallId};
use rivet_plugin::{PluginLoader, PluginSource};
use rivet_runtime::{BroadcastBus, Registry};
use rivet_telemetry_log::{
    CONVERSATION_TOPIC, DEFAULT_TOPICS, PLUGIN_ID, SUBSCRIBER_NAME, TelemetryLogPlugin,
};

fn plugin_id() -> PluginId {
    PluginId::new(PLUGIN_ID).expect("a valid id")
}

fn source() -> PluginSource {
    PluginSource::builtin(
        "rivet-telemetry-log",
        rivet_telemetry_log::MANIFEST_TOML,
        |manifest| Arc::new(TelemetryLogPlugin::new(manifest)),
    )
}

/// The grant `rivet-cli`'s `Profile` computes, copied rather than imported.
///
/// `rivet-cli` is a binary crate and depending on it from here would invert the dependency
/// arrow. `the_default_topics_survive_every_shipped_profile` is the test that keeps this
/// copy honest — if the CLI narrows its list, the copy has to follow or the assertion here
/// stops describing the shipped profiles. (`rivet-cli`'s own e2e covers the real thing.)
fn profile_grant(narrowed: bool) -> PermissionSet {
    let subscribe = if narrowed {
        Some(
            TopicScope::new(
                [
                    "agent.request.",
                    "agent.run.",
                    "agent.turn.",
                    "job.",
                    "plugin.",
                    "runtime.",
                    "tool.",
                ]
                .iter()
                .map(|s| (*s).to_string()),
            )
            .expect("a valid scope"),
        )
    } else {
        None
    };
    PermissionSet::new([
        Permission::FsRead(FsScope::Workspace),
        Permission::SessionRead,
        Permission::SessionWrite,
        Permission::EventsPublish,
        Permission::NetworkHttp(None),
        Permission::EventsSubscribe(subscribe),
    ])
}

/// Load the plugin with `config` under `grant`.
async fn load(
    grant: PermissionSet,
    config: serde_json::Value,
) -> (rivet_core::Result<()>, BroadcastBus) {
    let bus = BroadcastBus::new();
    let registry = Registry::new(bus.clone());
    let events = registry.events();
    let mut loader = PluginLoader::new(registry, events, rivet_core::ABI_VERSION, grant);
    loader.discover(&[source()]).expect("discover");
    loader.validate();
    let result = loader.load(&plugin_id(), config).await;
    // The loader is dropped here; the registry and its pump outlive it through `bus`.
    std::mem::forget(loader);
    (result, bus)
}

#[tokio::test]
async fn the_default_topics_survive_every_shipped_profile() {
    // `developer` and `ci` grant every topic; `readonly`, `reviewer` and `production`
    // grant seven prefixes. The six defaults sit inside both, so a telemetry plugin nobody
    // configured loads wherever it is enabled -- which is what makes "no config needed"
    // true rather than true-on-a-developer-laptop.
    for narrowed in [false, true] {
        let (result, _bus) = load(profile_grant(narrowed), serde_json::Value::Null).await;
        result.unwrap_or_else(|e| panic!("narrowed={narrowed}: {e}"));
    }

    // And spelled out at the value level, so a change to either list is visible here.
    let narrowed = profile_grant(true);
    for topic in DEFAULT_TOPICS {
        assert!(
            narrowed.allows(&Permission::EventsSubscribe(Some(
                TopicScope::new([topic.to_string()]).unwrap()
            ))),
            "`{topic}` is in the default subscription but not in the narrowed grant"
        );
    }
}

#[tokio::test]
async fn a_readonly_profile_refuses_a_telemetry_plugin_that_was_told_to_log_the_conversation() {
    // The guard cannot catch this: prefixes meet as a union, so six of the seven survive
    // and the meet is not empty. The plugin catches it, because the operator *wrote*
    // `include_conversation = true` and the answer is no.
    let (result, _bus) = load(
        profile_grant(true),
        serde_json::json!({ "include_conversation": true }),
    )
    .await;
    let error = result.expect_err("a narrowed profile has to refuse this, loudly");
    assert!(
        error.to_string().contains(CONVERSATION_TOPIC),
        "the message has to name what went missing: {error}"
    );
}

#[tokio::test]
async fn a_developer_profile_allows_the_conversation_when_asked() {
    // The other half: the refusal above is the profile's doing, not a blanket ban.
    let (result, _bus) = load(
        profile_grant(false),
        serde_json::json!({ "include_conversation": true }),
    )
    .await;
    result.expect("an unscoped grant covers `agent.text.`");
}

#[tokio::test]
async fn configured_topics_the_profile_narrows_are_refused_not_silently_dropped() {
    let (result, _bus) = load(
        profile_grant(true),
        serde_json::json!({ "topics": ["tool.", "agent.text."] }),
    )
    .await;
    let error = result.expect_err("a written list is a promise");
    assert!(error.to_string().contains("agent.text."), "{error}");
    assert!(
        error.to_string().contains(PLUGIN_ID),
        "an operator has to know which config table to edit: {error}"
    );
}

#[tokio::test]
async fn the_default_list_narrowed_by_a_grant_loads_anyway() {
    // The other side of the asymmetry: nobody wrote the default down, so it is a
    // preference and a narrower grant simply wins.
    let grant = PermissionSet::new([Permission::EventsSubscribe(Some(
        TopicScope::new(["tool.".to_string()]).unwrap(),
    ))]);
    let (result, _bus) = load(grant, serde_json::Value::Null).await;
    result.expect("the default list bends; a configured one does not");
}

// --- what it actually logs ------------------------------------------------------------------

/// A `tracing` layer that keeps the fields of every event it sees.
///
/// A newtype around the `Arc`, because a foreign trait cannot be implemented for
/// `Arc<Captured>` directly.
#[derive(Clone, Debug, Default)]
struct CaptureLayer(Arc<Captured>);

/// The records themselves, shared with the test that reads them.
#[derive(Debug, Default)]
struct Captured {
    records: Mutex<Vec<Vec<(String, String)>>>,
}

impl Captured {
    /// This plugin's own records. The host emits `tracing` events of its own -- the
    /// guard's narrowing note, for one -- and they are not what these tests are about.
    fn records(&self) -> Vec<Vec<(String, String)>> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|record| Self::field(record, "message").as_deref() == Some("rivet event"))
            .cloned()
            .collect()
    }

    fn field(record: &[(String, String)], name: &str) -> Option<String> {
        record
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

impl<S> tracing_subscriber::Layer<S> for CaptureLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Visit(Vec<(String, String)>);
        impl tracing::field::Visit for Visit {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0
                    .push((field.name().to_string(), format!("{value:?}")));
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.push((field.name().to_string(), value.to_string()));
            }
        }
        let mut visit = Visit(Vec::new());
        event.record(&mut visit);
        self.0
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(visit.0);
    }
}

/// Phase 3 `DoD` 4's other half: not only that the subscriber runs, but that what it emits
/// is a structured record rather than a line of prose.
#[tokio::test]
async fn the_telemetry_plugin_logs_what_it_receives() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let captured = Arc::new(Captured::default());
    let _guard = tracing_subscriber::registry()
        .with(CaptureLayer(captured.clone()))
        .set_default();

    let (result, bus) = load(profile_grant(false), serde_json::Value::Null).await;
    result.expect("load");

    let session = rivet_core::id::SessionId::new();
    let run = rivet_core::id::RunId::new();
    bus.publish(
        EventEnvelope::new(Event::Tool(ToolEvent::Started {
            call_id: ToolCallId::new(),
            name: "read_file".into(),
            sandboxed: false,
        }))
        .for_run(session, run),
    );

    for _ in 0..200 {
        if !captured.records().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let records = captured.records();
    let record = records
        .iter()
        .find(|r| Captured::field(r, "topic").as_deref() == Some("tool.execute.started"))
        .unwrap_or_else(|| panic!("no record for the published event: {records:?}"));
    assert_eq!(
        Captured::field(record, "subject").as_deref(),
        Some("read_file")
    );
    assert_eq!(
        Captured::field(record, "run").as_deref(),
        Some(run.to_string()).as_deref()
    );
}

/// Every record carries the same five correlation fields — that is the whole point of a
/// structured log as against the JSONL stream.
#[tokio::test]
async fn every_log_record_names_its_topic_and_its_run() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let captured = Arc::new(Captured::default());
    let _guard = tracing_subscriber::registry()
        .with(CaptureLayer(captured.clone()))
        .set_default();

    let (result, bus) = load(profile_grant(false), serde_json::Value::Null).await;
    result.expect("load");

    let session = rivet_core::id::SessionId::new();
    let run = rivet_core::id::RunId::new();
    for payload in [
        Event::Tool(ToolEvent::Blocked {
            call_id: ToolCallId::new(),
            reason: "out of scope".into(),
        }),
        Event::Agent(AgentEvent::TurnStarted { turn: 2 }),
    ] {
        bus.publish(EventEnvelope::new(payload).for_run(session, run));
    }

    for _ in 0..200 {
        if captured.records().len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let records = captured.records();
    assert!(records.len() >= 2, "{records:?}");
    for record in &records {
        for field in ["topic", "event_id", "session", "run", "at"] {
            assert!(
                Captured::field(record, field).is_some(),
                "`{field}` missing from {record:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_subscriber_registers_under_its_own_name() {
    // The name a `runtime.subscriber.lagged` report will carry. The host wraps the
    // subscriber to enforce the topic filter, and the wrapper must not rename it.
    let bus = BroadcastBus::new();
    let registry = Registry::new(bus.clone());
    let events = registry.events();
    let mut loader = PluginLoader::new(
        registry,
        events,
        rivet_core::ABI_VERSION,
        profile_grant(false),
    );
    loader.discover(&[source()]).expect("discover");
    loader.validate();
    loader
        .load(&plugin_id(), serde_json::Value::Null)
        .await
        .expect("load");

    assert_eq!(
        loader.record(&plugin_id()).unwrap().registered,
        [format!("subscriber:{SUBSCRIBER_NAME}")]
    );
}
