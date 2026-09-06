//! The loader, driven through its whole state machine.
//!
//! The first five tests are Phase 2's five acceptance lines, one each, named so that
//! `docs/plan.md` can cite them.

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::event::{Event, EventEnvelope, EventSubscriber};
use rivet_core::plugin::PluginState;
use support::{
    After, id, loader, manifest_toml, no_config, no_grant, null_config, source, tool_source,
};

// --- DoD 1: a failed load leaves nothing ---------------------------------------------------

#[tokio::test]
async fn a_plugin_that_fails_after_registering_leaves_nothing() {
    let good = tool_source("good", "test.good", &["kept"], After::Succeed);
    let bad = tool_source(
        "bad",
        "test.fails-after-registering",
        &["gone_a", "gone_b"],
        After::Fail,
    );
    let (mut loader, registry, _bus) = loader(no_grant());
    loader.discover(&[good, bad]).unwrap();
    loader.validate();

    loader.load(&id("test.good"), null_config()).await.unwrap();
    let error = loader
        .load(&id("test.fails-after-registering"), null_config())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("fails after registering"),
        "{error}"
    );

    assert_eq!(
        registry.tool_names().await,
        ["kept"],
        "both of the failing plugin's tools must be gone, and only its own"
    );
    let record = loader.record(&id("test.fails-after-registering")).unwrap();
    assert_eq!(record.state, PluginState::Failed);
    assert!(record.registered.is_empty(), "{:?}", record.registered);
    assert!(record.instance_id.is_none());
    assert!(
        record.error.is_some(),
        "the reason is retained for `plugin list`"
    );
}

#[tokio::test]
async fn a_plugin_that_panics_after_registering_leaves_nothing() {
    // A panic is not a nicer failure than an `Err`; it must take the same rollback path
    // rather than the process.
    let bad = tool_source("panicky", "test.panics", &["gone"], After::Panic);
    let (mut loader, registry, _bus) = loader(no_grant());
    loader.discover(&[bad]).unwrap();
    loader.validate();

    let error = loader
        .load(&id("test.panics"), null_config())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("panicked"), "{error}");
    assert!(registry.tool_names().await.is_empty());
    assert_eq!(
        loader.record(&id("test.panics")).unwrap().state,
        PluginState::Failed
    );
}

// --- DoD 2: an incompatible ABI is rejected before load --------------------------------

#[tokio::test]
async fn an_incompatible_abi_is_rejected_before_load_is_called() {
    let spy = support::plan("test.wrong-abi", &["never"], After::Succeed);
    let toml = manifest_toml("test.wrong-abi", "\"tool\"", "").replace("\"0.1\"", "\"0.99\"");
    let (mut loader, registry, _bus) = loader(no_grant());
    loader.discover(&[source("wrong-abi", toml)]).unwrap();
    loader.validate();

    let record = loader.record(&id("test.wrong-abi")).unwrap();
    assert_eq!(record.state, PluginState::Failed);
    let error = record.error.clone().unwrap();
    assert!(error.contains("0.99"), "{error}");
    assert!(error.contains("0.1"), "the host's version too: {error}");

    assert!(
        !spy.entered.load(Ordering::SeqCst),
        "`load` must never run: that is what `before registering` means"
    );
    assert!(registry.tool_names().await.is_empty());

    // And it stays rejected: `load` refuses a FAILED record rather than retrying it.
    let error = loader
        .load(&id("test.wrong-abi"), null_config())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("failed"), "{error}");
    assert!(!spy.entered.load(Ordering::SeqCst));
}

// --- DoD 3: a readonly profile strips write permission ---------------------------------

#[tokio::test]
async fn a_readonly_profile_strips_write_permission() {
    let toml = manifest_toml(
        "test.writer",
        "\"tool\"",
        "[[permissions]]\n\
         permission = \"fs_read\"\n\
         scope = \"workspace\"\n\n\
         [[permissions]]\n\
         permission = \"fs_write\"\n\
         scope = \"workspace\"\n",
    );
    support::plan("test.writer", &["w"], After::Succeed);

    let readonly = PermissionSet::new([Permission::FsRead(FsScope::Workspace)]);
    let (mut loader, _registry, _bus) = loader(readonly);
    loader.discover(&[source("writer", toml)]).unwrap();
    loader.validate();

    let record = loader.record(&id("test.writer")).unwrap();
    assert_eq!(record.state, PluginState::Validated);
    assert!(
        record
            .effective
            .allows(&Permission::FsRead(FsScope::Workspace)),
        "reading survives"
    );
    assert!(
        !record
            .effective
            .allows(&Permission::FsWrite(FsScope::Workspace)),
        "writing does not: {:?}",
        record.effective
    );
    assert_eq!(
        record.denied,
        [Permission::FsWrite(FsScope::Workspace)],
        "and the record says which request the profile removed"
    );
}

// --- DoD 4: a name collision names both plugins ----------------------------------------

#[tokio::test]
async fn a_name_collision_names_both_plugins() {
    let first = tool_source("first", "test.first", &["shell"], After::Succeed);
    let second = tool_source("second", "test.second", &["shell"], After::Succeed);
    let (mut loader, registry, _bus) = loader(no_grant());
    loader.discover(&[first, second]).unwrap();
    loader.validate();

    loader.load(&id("test.first"), null_config()).await.unwrap();
    let error = loader
        .load(&id("test.second"), null_config())
        .await
        .unwrap_err();

    let message = error.to_string();
    assert!(message.contains("test.first"), "{message}");
    assert!(message.contains("test.second"), "{message}");

    assert_eq!(
        registry.tool_names().await,
        ["shell"],
        "the first plugin keeps its registration"
    );
    assert_eq!(
        loader.record(&id("test.first")).unwrap().state,
        PluginState::Loaded
    );
    assert_eq!(
        loader.record(&id("test.second")).unwrap().state,
        PluginState::Failed
    );
}

// --- DoD 5: reload under the same name --------------------------------------------------

#[tokio::test]
async fn a_plugin_reloads_under_the_same_name() {
    let src = tool_source("reload", "test.reload", &["reloadable"], After::Succeed);
    let (mut loader, registry, _bus) = loader(no_grant());
    loader.discover(&[src]).unwrap();
    loader.validate();

    loader
        .load(&id("test.reload"), null_config())
        .await
        .unwrap();
    let first = loader
        .record(&id("test.reload"))
        .unwrap()
        .instance_id
        .unwrap();
    assert!(registry.tool("reloadable").await.is_some());

    loader.unload(&id("test.reload")).await.unwrap();
    let record = loader.record(&id("test.reload")).unwrap();
    assert_eq!(record.state, PluginState::Unloaded);
    assert!(record.registered.is_empty());
    assert!(
        registry.tool("reloadable").await.is_none(),
        "unload has to free the name, or reload cannot succeed"
    );

    loader
        .load(&id("test.reload"), null_config())
        .await
        .expect("re-registration after unload must succeed");
    let second = loader
        .record(&id("test.reload"))
        .unwrap()
        .instance_id
        .unwrap();
    assert_ne!(
        first, second,
        "a reload is a new instance, not a revived one"
    );
    assert!(registry.tool("reloadable").await.is_some());
}

// --- the guard --------------------------------------------------------------------------

#[tokio::test]
async fn registering_into_an_undeclared_slot_is_refused_and_rolled_back() {
    let src = tool_source(
        "undeclared",
        "test.undeclared",
        &["declared_tool"],
        After::RegisterUndeclaredPolicy,
    );
    let (mut loader, registry, _bus) = loader(no_grant());
    loader.discover(&[src]).unwrap();
    loader.validate();

    let error = loader
        .load(&id("test.undeclared"), null_config())
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("policy"), "{message}");
    assert!(message.contains("test.undeclared"), "{message}");
    assert!(
        message.contains("tool"),
        "the declared list is named too: {message}"
    );

    assert!(registry.policies().await.is_empty());
    assert!(
        registry.tool_names().await.is_empty(),
        "and the tool it did register goes with it"
    );
}

#[tokio::test]
async fn a_registration_from_a_task_outliving_a_failed_load_is_refused() {
    // The rollback has to be a seal, not a sweep. `PluginContext` is `Clone` and its
    // registry is an `Arc`, so a plugin that armed a task before failing still holds a
    // live registration handle after `unregister_all` has run. A capability that lands
    // then is owned by a dead instance id: no `unload` will ever mention it, and neither
    // `rivet doctor` nor `rivet plugin list` can see it, because both read
    // `record.registered`.
    let src = tool_source(
        "late",
        "test.late",
        &["gone"],
        After::FailAfterArmingALateRegistration,
    );
    let spy = support::plan(
        "test.late",
        &["gone"],
        After::FailAfterArmingALateRegistration,
    );
    let (mut loader, registry, _bus) = loader(no_grant());
    loader.discover(&[src]).unwrap();
    loader.validate();

    loader
        .load(&id("test.late"), null_config())
        .await
        .unwrap_err();
    assert!(registry.tool_names().await.is_empty(), "the rollback ran");

    let refusal = support::late_outcome(&spy)
        .await
        .expect_err("registering after the load window closed must fail, and loudly");
    assert!(
        refusal.contains("test.late"),
        "the error names the plugin: {refusal}"
    );
    assert!(
        refusal.contains("load"),
        "and says when the window was open: {refusal}"
    );

    assert!(
        registry.tool_names().await.is_empty(),
        "nothing may appear after the rollback: it would be unremovable"
    );
    let record = loader.record(&id("test.late")).unwrap();
    assert_eq!(record.state, PluginState::Failed);
    assert!(record.registered.is_empty(), "{:?}", record.registered);
    loader.unload_all().await;
    assert!(registry.tool_names().await.is_empty());
}

#[tokio::test]
async fn a_plugin_that_registers_from_unload_does_not_break_its_own_reload() {
    // `unload` runs *after* `unregister_all`, so a registration made there would survive
    // the teardown and hold the name against the next load -- DoD 5 defeated by the
    // plugin's own teardown, with the record reporting `UNLOADED` and no registrations
    // the whole time.
    let src = tool_source(
        "on-unload",
        "test.on-unload",
        &["t"],
        After::RegisterFromUnload,
    );
    let spy = support::plan("test.on-unload", &["t"], After::RegisterFromUnload);
    let (mut loader, registry, _bus) = loader(no_grant());
    loader.discover(&[src]).unwrap();
    loader.validate();
    loader
        .load(&id("test.on-unload"), null_config())
        .await
        .unwrap();

    loader.unload(&id("test.on-unload")).await.unwrap();
    let refusal = spy
        .late_outcome()
        .expect("unload ran and attempted its registration")
        .expect_err("the window is closed before `unload` is called");
    assert!(
        refusal.contains("test.on-unload"),
        "the error names the plugin: {refusal}"
    );
    assert!(
        registry.tool_names().await.is_empty(),
        "otherwise the name is held by an instance that no longer exists"
    );

    loader
        .load(&id("test.on-unload"), null_config())
        .await
        .expect("re-registration after unload must succeed");
    assert_eq!(registry.tool_names().await, ["t"]);
}

#[tokio::test]
async fn what_a_plugin_claims_is_checked_against_what_it_registered() {
    // The handle is a claim; the guard's list is the fact.
    let src = tool_source("claims", "test.claims", &["a", "b"], After::Succeed);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[src]).unwrap();
    loader.validate();
    loader
        .load(&id("test.claims"), null_config())
        .await
        .unwrap();

    let record = loader.record(&id("test.claims")).unwrap();
    assert_eq!(record.registered, ["tool:a", "tool:b"]);
    assert!(record.claim_matches_reality());
}

// --- discovery and state machine ---------------------------------------------------------

#[test]
fn two_sources_claiming_one_id_name_both_origins() {
    let first = tool_source("dup-one", "test.duplicate", &["x"], After::Succeed);
    let second = source("dup-two", manifest_toml("test.duplicate", "\"tool\"", ""));
    let (mut loader, _registry, _bus) = loader(no_grant());
    let error = loader.discover(&[first, second]).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("builtin(dup-one)"), "{message}");
    assert!(message.contains("builtin(dup-two)"), "{message}");
}

#[tokio::test]
async fn loading_an_id_nobody_discovered_lists_the_ones_that_exist() {
    let src = tool_source("known", "test.known", &["k"], After::Succeed);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[src]).unwrap();
    loader.validate();

    let error = loader
        .load(&id("test.absent"), null_config())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), rivet_core::error::ErrorKind::NotFound);
    assert!(error.to_string().contains("test.known"), "{error}");
}

#[tokio::test]
async fn loading_an_already_loaded_plugin_is_refused() {
    let src = tool_source("twice", "test.twice", &["t"], After::Succeed);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[src]).unwrap();
    loader.validate();
    loader.load(&id("test.twice"), null_config()).await.unwrap();

    let error = loader
        .load(&id("test.twice"), null_config())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("LOADED"), "{error}");
    assert!(error.to_string().contains("unload"), "{error}");
}

#[tokio::test]
async fn unloading_something_that_is_not_loaded_is_not_found() {
    let src = tool_source("never", "test.never-loaded", &["n"], After::Succeed);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[src]).unwrap();
    loader.validate();

    let error = loader.unload(&id("test.never-loaded")).await.unwrap_err();
    assert_eq!(error.kind(), rivet_core::error::ErrorKind::NotFound);
}

#[tokio::test]
async fn a_batch_becomes_active_only_when_every_plugin_loaded() {
    let ok = tool_source("batch-ok", "test.batch-ok", &["ok"], After::Succeed);
    let broken = tool_source("batch-bad", "test.batch-bad", &["bad"], After::Fail);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[ok, broken]).unwrap();
    loader.validate();

    let report = loader
        .load_selected(&[id("test.batch-ok"), id("test.batch-bad")], &no_config)
        .await;
    assert_eq!(report.loaded, [id("test.batch-ok")]);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.registered, ["tool:ok"]);
    assert_eq!(
        loader.record(&id("test.batch-ok")).unwrap().state,
        PluginState::Loaded,
        "left at LOADED, visibly one the host is about to tear down"
    );
}

#[tokio::test]
async fn a_second_batch_does_not_promote_the_records_of_the_first() {
    // `ACTIVE` means "the host accepted the whole batch", and a record left at `LOADED`
    // means "the host is about to tear this down". A sweep over every record makes both
    // false for the first batch the moment a second one succeeds.
    let ok = tool_source("two-a", "test.two-a", &["a2"], After::Succeed);
    let broken = tool_source("two-b", "test.two-b", &["b2"], After::Fail);
    let later = tool_source("two-c", "test.two-c", &["c2"], After::Succeed);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[ok, broken, later]).unwrap();
    loader.validate();

    let first = loader
        .load_selected(&[id("test.two-a"), id("test.two-b")], &no_config)
        .await;
    assert_eq!(first.failed.len(), 1);
    assert_eq!(
        loader.record(&id("test.two-a")).unwrap().state,
        PluginState::Loaded
    );

    let second = loader.load_selected(&[id("test.two-c")], &no_config).await;
    assert!(second.failed.is_empty(), "{:?}", second.failed);
    assert_eq!(
        loader.record(&id("test.two-c")).unwrap().state,
        PluginState::Active,
        "this batch committed"
    );
    assert_eq!(
        loader.record(&id("test.two-a")).unwrap().state,
        PluginState::Loaded,
        "and the batch that did not stays where the host left it"
    );
}

#[tokio::test]
async fn a_batch_with_nothing_failing_commits_to_active() {
    let ok = tool_source("batch-ok2", "test.batch-ok2", &["ok2"], After::Succeed);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[ok]).unwrap();
    loader.validate();

    let report = loader
        .load_selected(&[id("test.batch-ok2")], &no_config)
        .await;
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(
        loader.record(&id("test.batch-ok2")).unwrap().state,
        PluginState::Active
    );
}

#[tokio::test]
async fn a_batch_attempts_every_plugin_rather_than_stopping_at_the_first_failure() {
    // `rivet doctor` exists to report what is broken; dying on the first problem is the
    // one thing it must not do.
    let a = tool_source("multi-a", "test.multi-a", &["a"], After::Fail);
    let b = tool_source("multi-b", "test.multi-b", &["b"], After::Fail);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[a, b]).unwrap();
    loader.validate();

    let report = loader
        .load_selected(&[id("test.multi-a"), id("test.multi-b")], &no_config)
        .await;
    assert_eq!(report.failed.len(), 2, "both failures are reported");
}

#[tokio::test]
async fn a_duplicated_id_in_a_batch_loads_once() {
    let src = tool_source("dedup", "test.dedup", &["d"], After::Succeed);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[src]).unwrap();
    loader.validate();

    let report = loader
        .load_selected(&[id("test.dedup"), id("test.dedup")], &no_config)
        .await;
    assert_eq!(report.loaded, [id("test.dedup")]);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
}

#[tokio::test]
async fn unload_all_calls_every_plugins_unload() {
    let a = tool_source("all-a", "test.all-a", &["aa"], After::Succeed);
    let b = tool_source("all-b", "test.all-b", &["bb"], After::Succeed);
    let spy_a = support::plan("test.all-a", &["aa"], After::Succeed);
    let spy_b = support::plan("test.all-b", &["bb"], After::Succeed);
    let (mut loader, registry, _bus) = loader(no_grant());
    loader.discover(&[a, b]).unwrap();
    loader.validate();
    loader
        .load_selected(&[id("test.all-a"), id("test.all-b")], &no_config)
        .await;

    loader.unload_all().await;
    assert_eq!(spy_a.unloads.load(Ordering::SeqCst), 1);
    assert_eq!(spy_b.unloads.load(Ordering::SeqCst), 1);
    assert!(registry.tool_names().await.is_empty());
}

// --- cancellation ------------------------------------------------------------------------

#[tokio::test]
async fn unloading_one_plugin_does_not_cancel_anothers_token() {
    // A single shared token would stop every plugin's background work the moment one
    // plugin unloaded.
    let a = tool_source("tok-a", "test.tok-a", &["ta"], After::Succeed);
    let b = tool_source("tok-b", "test.tok-b", &["tb"], After::Succeed);
    let spy_a = support::plan("test.tok-a", &["ta"], After::Succeed);
    let spy_b = support::plan("test.tok-b", &["tb"], After::Succeed);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[a, b]).unwrap();
    loader.validate();
    loader
        .load_selected(&[id("test.tok-a"), id("test.tok-b")], &no_config)
        .await;

    loader.unload(&id("test.tok-a")).await.unwrap();
    assert!(spy_a.token_cancelled());
    assert!(
        !spy_b.token_cancelled(),
        "the sibling's background work must survive"
    );

    loader.shutdown();
    assert!(
        spy_b.token_cancelled(),
        "and the root token stops everything"
    );
}

#[tokio::test]
async fn a_failed_loads_token_is_cancelled_with_its_registrations() {
    let src = tool_source("tok-fail", "test.tok-fail", &["tf"], After::Fail);
    let spy = support::plan("test.tok-fail", &["tf"], After::Fail);
    let (mut loader, _registry, _bus) = loader(no_grant());
    loader.discover(&[src]).unwrap();
    loader.validate();

    loader
        .load(&id("test.tok-fail"), null_config())
        .await
        .unwrap_err();
    assert!(
        spy.token_cancelled(),
        "nobody downstream will ever cancel it: the load returned Err"
    );
}

// --- events ------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct TopicLog(std::sync::Mutex<Vec<String>>);

#[rivet_core::prelude::async_trait]
impl EventSubscriber for TopicLog {
    fn name(&self) -> &'static str {
        "topics"
    }

    async fn on_event(&self, envelope: &EventEnvelope) {
        if matches!(envelope.payload, Event::Plugin(_)) {
            self.0.lock().unwrap().push(envelope.topic().to_string());
        }
    }
}

#[tokio::test]
async fn the_lifecycle_is_published_on_the_bus() {
    let ok = tool_source("evt-ok", "test.evt-ok", &["e"], After::Succeed);
    let bad = tool_source("evt-bad", "test.evt-bad", &["f"], After::Fail);
    let (mut loader, _registry, bus) = loader(no_grant());
    let log = Arc::new(TopicLog::default());
    let pump = bus.attach(log.clone());

    loader.discover(&[ok, bad]).unwrap();
    loader.validate();
    loader
        .load_selected(&[id("test.evt-ok"), id("test.evt-bad")], &no_config)
        .await;
    loader.unload(&id("test.evt-ok")).await.unwrap();

    // The bus is lossy by design, so poll rather than assert on the first look.
    for _ in 0..100 {
        if log.0.lock().unwrap().len() >= 5 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    pump.abort();

    let seen = log.0.lock().unwrap().clone();
    for topic in [
        "plugin.discovered",
        "plugin.loaded",
        "plugin.load.failed",
        "plugin.unloaded",
    ] {
        assert!(seen.iter().any(|t| t == topic), "{topic} missing: {seen:?}");
    }
}
