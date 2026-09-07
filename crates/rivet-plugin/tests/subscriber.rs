//! Subscriber registration through the real loader: `DoD` 4, `DoD` 5, and the topic filter.
//!
//! Everything here goes through `PluginLoader`, not through `effective_topics` directly.
//! The unit tests next to that function cover the lattice; these cover the wiring — that a
//! manifest's `events_subscribe` reaches the guard, that the guard's answer reaches the
//! bus, and that an unload takes the pump with it.

mod support;

use std::sync::Arc;

use rivet_core::capability::{FsScope, Permission, PermissionSet, TopicScope};
use rivet_core::event::{AgentEvent, Event, EventBus, EventEnvelope, ToolEvent};
use rivet_core::id::ToolCallId;
use rivet_core::plugin::PluginState;
use rivet_runtime::BroadcastBus;
use support::{SinkWatch, events_subscribe, id, loader, no_config, subscriber_source};

/// Everything a profile grants: the `developer` shape, unscoped.
fn full_grant() -> PermissionSet {
    PermissionSet::new([Permission::EventsSubscribe(None)])
}

/// A grant scoped to `prefixes`.
fn scoped_grant(prefixes: &[&str]) -> PermissionSet {
    PermissionSet::new([Permission::EventsSubscribe(Some(
        TopicScope::new(prefixes.iter().map(|p| (*p).to_string())).expect("a valid scope"),
    ))])
}

/// The `readonly` profile's real grant, as `rivet-cli` computes it: everything except the
/// model's own output.
fn readonly_grant() -> PermissionSet {
    PermissionSet::new([
        Permission::FsRead(FsScope::Workspace),
        Permission::EventsSubscribe(Some(
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
            .unwrap(),
        )),
    ])
}

fn tool_event() -> EventEnvelope {
    EventEnvelope::new(Event::Tool(ToolEvent::Blocked {
        call_id: ToolCallId::new(),
        reason: "denied".into(),
    }))
}

fn text_event() -> EventEnvelope {
    EventEnvelope::new(Event::Agent(AgentEvent::TextDelta { text: "hi".into() }))
}

/// Load one subscriber plugin and return the bus it is attached to.
async fn load_one(
    crate_name: &'static str,
    plugin_id: &str,
    wants: &[&str],
    permissions: &str,
    grant: PermissionSet,
) -> (rivet_plugin::PluginLoader, BroadcastBus, SinkWatch) {
    let (source, watch) = subscriber_source(crate_name, plugin_id, wants, permissions);
    let (mut loader, _registry, bus) = loader(grant);
    loader.discover(&[source]).expect("discover");
    loader.validate();
    let report = loader.load_selected(&[id(plugin_id)], &no_config).await;
    assert!(
        report.failed.is_empty(),
        "expected the plugin to load: {:?}",
        report.failed
    );
    (loader, bus, watch)
}

/// The error a load failed with, for the tests that expect a refusal.
async fn refusal(
    crate_name: &'static str,
    plugin_id: &str,
    wants: &[&str],
    permissions: &str,
    grant: PermissionSet,
) -> String {
    let (source, _watch) = subscriber_source(crate_name, plugin_id, wants, permissions);
    let (mut loader, _registry, _bus) = loader(grant);
    loader.discover(&[source]).expect("discover");
    loader.validate();
    let error = loader
        .load(&id(plugin_id), serde_json::Value::Null)
        .await
        .expect_err("the guard must refuse this registration");
    assert_eq!(
        loader.record(&id(plugin_id)).unwrap().state,
        PluginState::Failed,
        "a refused registration is a failed load, not a quiet no-op"
    );
    error.to_string()
}

// --- DoD 4: a registered subscriber plugin actually receives events ---------------------------

/// Phase 3 `DoD` 4.
///
/// Registration *is* attachment — that was Phase 0's decision, made because two separate
/// calls let a caller register a subscriber that silently received nothing. Nothing proved
/// it through a real plugin until here.
#[tokio::test]
async fn a_registered_subscriber_plugin_receives_events() {
    let (loader, bus, watch) = load_one(
        "sub-receives",
        "test.sub-receives",
        &[],
        &events_subscribe(None),
        full_grant(),
    )
    .await;

    bus.publish(tool_event());
    assert!(watch.wait_for(1).await, "the plugin's sink got nothing");
    // `plugin.loaded` is in there too: registration is attachment, and the loader
    // publishes that event after `load` returns. The point is that delivery is live.
    assert!(watch.seen().contains(&"tool.blocked".to_string()));

    assert_eq!(
        loader.record(&id("test.sub-receives")).unwrap().registered,
        ["subscriber:sink"],
        "and the guard recorded it under the plugin's own name"
    );
}

#[tokio::test]
async fn a_subscribers_own_name_survives_the_wrapper() {
    // The registry keys subscribers by name and a lag report names them, so a wrapper that
    // renamed anything would show a plugin author a name they never chose.
    let (loader, _bus, _watch) = load_one(
        "sub-name",
        "test.sub-name",
        &["tool."],
        &events_subscribe(Some(&["tool.", "plugin."])),
        scoped_grant(&["tool.", "plugin."]),
    )
    .await;
    assert_eq!(
        loader.record(&id("test.sub-name")).unwrap().registered,
        ["subscriber:sink"]
    );
}

// --- DoD 5: an unloaded plugin stops observing -------------------------------------------------

/// Phase 3 `DoD` 5, at the delivery end.
#[tokio::test]
async fn an_unloaded_plugins_subscription_task_stops() {
    let (mut loader, bus, watch) = load_one(
        "sub-unload",
        "test.sub-unload",
        &[],
        &events_subscribe(None),
        full_grant(),
    )
    .await;

    bus.publish(tool_event());
    assert!(watch.wait_for(1).await, "nothing arrived before the unload");

    loader.unload(&id("test.sub-unload")).await.expect("unload");

    let before = watch.seen().len();
    for _ in 0..5 {
        bus.publish(tool_event());
    }
    // Give the pump every chance to deliver, then assert it did not.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        watch.seen().len(),
        before,
        "an unloaded plugin kept observing: {:?}",
        watch.seen()
    );
}

/// Phase 3 `DoD` 5, at the task end — which is the claim the `DoD` actually makes.
///
/// "Delivery stopped" and "the task ended" are different statements. A pump parked forever
/// inside `on_event` would satisfy the first and not the second, and it would hold the
/// plugin's subscriber alive for the life of the process. The `Drop` flag is what tells
/// them apart.
#[tokio::test]
async fn an_unloaded_subscriber_is_dropped_not_merely_silenced() {
    let (mut loader, bus, watch) = load_one(
        "sub-dropped",
        "test.sub-dropped",
        &[],
        &events_subscribe(None),
        full_grant(),
    )
    .await;
    bus.publish(tool_event());
    assert!(watch.wait_for(1).await);
    assert!(!watch.dropped(), "still registered, still alive");

    loader
        .unload(&id("test.sub-dropped"))
        .await
        .expect("unload");

    assert!(
        watch.wait_for_drop().await,
        "the pump task still holds the subscriber after unload"
    );
}

// --- 3.2: the filter -----------------------------------------------------------------------

#[tokio::test]
async fn a_plugin_without_events_subscribe_cannot_register_a_subscriber() {
    let message = refusal("sub-nogrant", "test.sub-nogrant", &[], "", full_grant()).await;
    assert!(message.contains("events_subscribe"), "{message}");
    assert!(message.contains("manifest"), "{message}");
}

/// The distinction `Grant` exists for. Same plugin, same registration, two failures whose
/// remedies are different — so the sentences are.
#[tokio::test]
async fn a_manifest_that_never_asked_and_a_profile_that_removed_it_say_different_things() {
    let never_asked = refusal("sub-never", "test.sub-never", &[], "", full_grant()).await;

    // The manifest asks for a scope the profile shares nothing with, so `validate` records
    // it in `denied` and `effective` loses it entirely.
    let removed = refusal(
        "sub-removed",
        "test.sub-removed",
        &[],
        &events_subscribe(Some(&["agent.text."])),
        scoped_grant(&["tool."]),
    )
    .await;

    assert!(never_asked.contains("manifest"), "{never_asked}");
    assert!(removed.contains("profile"), "{removed}");
    assert!(
        removed.contains("rivet plugin show"),
        "the guard does not know the profile's name, so it points at what does: {removed}"
    );
    assert_ne!(never_asked, removed);
}

#[tokio::test]
async fn an_empty_topics_list_becomes_the_grant_not_everything() {
    // The inversion this phase exists to fix: an empty `topics()` used to mean "every
    // topic" at the delivery end, so the least-privileged subscriber received the most.
    let (_loader, bus, watch) = load_one(
        "sub-empty",
        "test.sub-empty",
        &[],
        &events_subscribe(Some(&["tool."])),
        scoped_grant(&["tool.", "agent."]),
    )
    .await;

    bus.publish(text_event());
    bus.publish(tool_event());
    assert!(watch.wait_for(1).await);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        watch.seen().contains(&"tool.blocked".to_string()),
        "saw {:?}",
        watch.seen()
    );
    assert!(
        !watch.seen().iter().any(|t| t.starts_with("agent.")),
        "an empty preference has to mean the grant, not everything: {:?}",
        watch.seen()
    );
}

#[tokio::test]
async fn a_subscriber_narrower_than_its_grant_keeps_its_own_filter() {
    let (_loader, bus, watch) = load_one(
        "sub-narrow",
        "test.sub-narrow",
        &["tool."],
        &events_subscribe(None),
        full_grant(),
    )
    .await;

    bus.publish(text_event());
    bus.publish(tool_event());
    assert!(watch.wait_for(1).await);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        watch.seen(),
        ["tool.blocked"],
        "`plugin.loaded` is outside `tool.` too, so this list is exact"
    );
}

#[tokio::test]
async fn a_grant_narrower_than_the_subscriber_wins_with_the_longer_prefix() {
    // A grant of `tool.` and a request for `tool.execute.` meet at `tool.execute.` — the
    // longer prefix, which is exactly the set both allow. A string intersection would have
    // produced nothing here and punished the plugin for asking narrowly.
    let (_loader, bus, watch) = load_one(
        "sub-longer",
        "test.sub-longer",
        &["tool.execute."],
        &events_subscribe(None),
        scoped_grant(&["tool."]),
    )
    .await;

    bus.publish(tool_event()); // tool.blocked -- inside the grant, outside the request
    bus.publish(EventEnvelope::new(Event::Tool(ToolEvent::Completed {
        call_id: ToolCallId::new(),
        is_error: false,
        duration_ms: 1,
    })));
    assert!(watch.wait_for(1).await);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(watch.seen(), ["tool.execute.completed"]);
}

#[tokio::test]
async fn a_subscriber_whose_topics_fall_outside_its_grant_is_refused() {
    let message = refusal(
        "sub-outside",
        "test.sub-outside",
        &["job."],
        &events_subscribe(Some(&["tool."])),
        scoped_grant(&["tool."]),
    )
    .await;
    assert!(message.contains("job."), "both lists are named: {message}");
    assert!(message.contains("tool."), "{message}");
}

/// The test `docs/architecture.md` §11-10 is closed by.
#[tokio::test]
async fn a_readonly_profile_keeps_the_conversation_from_a_subscriber() {
    // A subscriber that asks for everything, under the profile that withholds the model's
    // output. Before Phase 3 it received `agent.text.delta` regardless.
    let (_loader, bus, watch) = load_one(
        "sub-readonly",
        "test.sub-readonly",
        &[],
        &events_subscribe(None),
        readonly_grant(),
    )
    .await;

    for _ in 0..5 {
        bus.publish(text_event());
    }
    bus.publish(tool_event());
    assert!(watch.wait_for(1).await);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert!(watch.seen().contains(&"tool.blocked".to_string()));
    assert!(
        !watch.seen().iter().any(|t| t.starts_with("agent.text")),
        "the conversation reached a subscriber the profile withholds it from: {:?}",
        watch.seen()
    );
}

#[tokio::test]
async fn two_subscribe_grants_join_rather_than_the_first_winning() {
    // A manifest may write `[[permissions]]` twice, and `PermissionSet`'s dedup folds only
    // equal values, so both survive into `effective`. Taking the first would narrow or
    // widen depending on sort order; the plugin holds both.
    let permissions = format!(
        "{}{}",
        events_subscribe(Some(&["tool."])),
        events_subscribe(Some(&["plugin."]))
    );
    let (_loader, bus, watch) =
        load_one("sub-join", "test.sub-join", &[], &permissions, full_grant()).await;

    bus.publish(text_event());
    bus.publish(tool_event());
    bus.publish(EventEnvelope::new(Event::Plugin(
        rivet_core::event::PluginEvent::Unloaded {
            plugin_id: id("test.sub-join"),
        },
    )));
    assert!(watch.wait_for(2).await, "saw {:?}", watch.seen());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let seen = watch.seen();
    assert!(seen.contains(&"tool.blocked".to_string()), "{seen:?}");
    assert!(seen.contains(&"plugin.unloaded".to_string()), "{seen:?}");
    assert!(
        !seen.iter().any(|t| t.starts_with("agent.")),
        "the join is the two scopes, not everything: {seen:?}"
    );
}

#[tokio::test]
async fn the_slot_check_and_the_permission_check_are_different_refusals() {
    // A manifest declaring only `tool` fails at the slot; one declaring the slot without
    // the permission fails at the grant. "What do you fill" and "what may you see" are two
    // questions, and an operator fixing the wrong one wastes a cycle.
    // Declares `tool`, registers a subscriber. `subscriber_source` plans the script; the
    // manifest here declares the wrong slot on purpose.
    let (_ignored, _watch) = subscriber_source("sub-slot", "test.sub-slot", &[], "");
    let source = support::source(
        "sub-slot",
        support::manifest_toml("test.sub-slot", "\"tool\"", &events_subscribe(None)),
    );
    let (mut loader, _registry, _bus) = loader(full_grant());
    loader.discover(&[source]).expect("discover");
    loader.validate();
    let slot_error = loader
        .load(&id("test.sub-slot"), serde_json::Value::Null)
        .await
        .expect_err("the manifest never declared the slot");

    assert!(
        slot_error.to_string().contains("manifest declares only"),
        "{slot_error}"
    );

    let permission_error = refusal("sub-slot2", "test.sub-slot2", &[], "", full_grant()).await;
    assert_ne!(slot_error.to_string(), permission_error);
}

/// `Arc<dyn EventSubscriber>` is what the registry stores, and a wrapper must not change
/// what a plugin sees itself as. Kept next to the wiring tests because the wrapper is only
/// observable from here.
#[tokio::test]
async fn a_wrapped_subscriber_reports_the_effective_topics() {
    use rivet_core::event::EventSubscriber;

    let inner: Arc<dyn EventSubscriber> = Arc::new(support::Sink::probe("inner", vec![]));
    let scoped = rivet_plugin::ScopedSubscriber::new(
        inner,
        Some(TopicScope::new(["tool.".to_string()]).unwrap()),
    );
    assert_eq!(scoped.name(), "inner");
    assert_eq!(scoped.topics(), ["tool."]);
}
