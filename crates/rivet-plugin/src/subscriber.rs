//! What a plugin's subscriber actually receives.
//!
//! Until Phase 3 the delivery path read no permission at all: `BroadcastBus::attach`
//! filtered on [`EventSubscriber::topics`], which is the subscriber's *own* preference and
//! defaults to the empty list — and the empty list means "everything". So a plugin that
//! declared `events_subscribe(["tool."])` received `agent.text.delta`, and a plugin that
//! declared nothing subscribed anyway. `docs/architecture.md` §11-10 is the entry that
//! named it; this module is what closes it.
//!
//! Two decisions carry the module.
//!
//! **The meet is the one that already exists.** [`Permission::meet`] knows that two topic
//! prefixes admit a common topic only when one is a prefix of the other, and that the
//! longer one is then the answer. That is the same function `manifest ∩ profile` runs
//! through at validation. Writing a second one here would put the same judgement in two
//! places, and one of them would eventually be fixed alone — which is the history
//! `TopicScope` was created out of.
//!
//! **An empty meet is an error, not a pass.** Handing the delivery path an empty list
//! would grant *everything* to a subscriber whose grant covers nothing. It fails exactly
//! backwards, so it fails loudly instead.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::capability::{Permission, PermissionSet, TopicScope};
use rivet_core::error::Error;
use rivet_core::event::{EventEnvelope, EventSubscriber};

/// What the loader decided about one plugin's permissions, in the shape the guard needs.
///
/// Two fields rather than one `PermissionSet`, because "the manifest never asked" and "the
/// profile took it away" are the *same absence* inside `effective` and two different
/// sentences to an operator: fix the manifest, or change `--profile`. The loader already
/// holds both ([`PluginRecord::effective`](crate::PluginRecord::effective) and
/// [`PluginRecord::denied`](crate::PluginRecord::denied)); this hands them over rather than
/// recomputing either.
///
/// A named type rather than two parameters so that the third term `plugin.md` §4.2
/// forecasts — `∩ session overrides` — does not break the signature again.
#[derive(Clone, Debug, Default)]
pub struct Grant {
    /// `manifest ∩ profile`.
    pub effective: PermissionSet,
    /// What the manifest asked for that the profile removed entirely.
    pub denied: Vec<Permission>,
}

impl Grant {
    /// Whether the profile is what removed `events_subscribe`.
    ///
    /// The distinction the error messages are built on: a `denied` entry means the
    /// manifest asked and the profile said no.
    fn profile_removed_subscribe(&self) -> bool {
        self.denied
            .iter()
            .any(|permission| matches!(permission, Permission::EventsSubscribe(_)))
    }
}

/// The topics a subscriber will actually receive: its own preference, met with its grant.
///
/// `Ok(None)` is "every topic" — the grant is unscoped and so is the request. `Ok(Some(_))`
/// is the filter to install. There is no `Ok` for "nothing": an empty list is read as
/// "everything" by [`rivet_core::event::topic_matches`], so a subscriber whose grant
/// overlaps its request in nothing must be refused rather than handed one.
///
/// # Errors
/// - The grant carries no `events_subscribe` at all — with two different messages, because
///   the operator's next move differs (see [`Grant`]).
/// - `wanted` is non-empty but not a valid topic scope. An empty prefix matches every
///   topic; treating that as "no preference" would silently *widen* what the plugin wrote.
/// - The meet is empty: the grant and the request share no topic.
pub fn effective_topics(
    grant: &Grant,
    wanted: &[String],
) -> rivet_core::Result<Option<TopicScope>> {
    let Some(granted) = join_subscribe_grants(grant.effective.granted()) else {
        return Err(no_subscribe_permission(grant));
    };

    // Only the *empty* request folds to "no preference". A non-empty list that will not
    // build a scope is the plugin's own mistake, and swallowing it would hand that plugin
    // the whole grant instead of the narrower thing it tried to write.
    let wanted = if wanted.is_empty() {
        Permission::EventsSubscribe(None)
    } else {
        Permission::EventsSubscribe(Some(TopicScope::new(wanted.to_vec())?))
    };

    match granted.meet(&wanted) {
        Some(Permission::EventsSubscribe(scope)) => Ok(scope),
        _ => Err(Error::plugin(format!(
            "this subscriber asked for {} and its grant covers {}; the two share no topic. \
             Prefixes overlap only when one contains the other.",
            describe(&wanted),
            describe(&granted)
        ))),
    }
}

/// Every `EventsSubscribe` in the grant, joined into one.
///
/// The rule and its reasoning live on [`Permission::join_events_subscribe`], in
/// `rivet-core` beside the `meet` it has to agree with. It is shared rather than local
/// because a plugin that reads its own grant needs the same answer and cannot depend on
/// this crate: `rivet.telemetry-log` sees only `rivet-core`, and its own copy of this loop
/// stopped at the first match.
fn join_subscribe_grants(granted: &[Permission]) -> Option<Permission> {
    Permission::join_events_subscribe(granted)
}

/// The two sentences of failure mode 4. The operator's next move is different, so the
/// message is.
fn no_subscribe_permission(grant: &Grant) -> Error {
    if grant.profile_removed_subscribe() {
        // The guard does not know the active profile's *name* — neither `PermissionSet`
        // nor `PluginLoader` carries one, and threading it in for one sentence is not
        // worth a constructor change. `rivet plugin show` does know it, and already prints
        // "removed by profile `<name>`".
        Error::plugin(
            "this plugin asked for `events_subscribe` and the active profile removed it, \
             so it cannot register a subscriber. Run `rivet plugin show <id>` to see which \
             profile, or pass a different `--profile`.",
        )
    } else {
        Error::plugin(
            "this plugin registered a subscriber without asking for `events_subscribe`; \
             add it to the manifest's `[[permissions]]`.",
        )
    }
}

fn describe(permission: &Permission) -> String {
    match permission {
        Permission::EventsSubscribe(None) => "every topic".to_string(),
        Permission::EventsSubscribe(Some(topics)) => format!("{:?}", topics.as_slice()),
        other => format!("{other:?}"),
    }
}

/// A subscriber whose `topics()` has been replaced by the effective list.
///
/// `name()` and `on_event` delegate. That matters twice over: the registry keys
/// subscribers by name, and a lag report names the subscriber — so wrapping must not
/// rename anything, or a plugin author reading `runtime.subscriber.lagged` would see a
/// name they never chose.
///
/// The filter is fixed here, at registration. A plugin holding its own `Arc` can change
/// what its `topics()` returns afterwards and it will not matter: the pump read this
/// wrapper's list once, before the first event.
pub struct ScopedSubscriber {
    inner: Arc<dyn EventSubscriber>,
    topics: Vec<String>,
}

impl ScopedSubscriber {
    /// Wrap `inner` so that it receives `topics` and nothing else.
    ///
    /// `None` is "every topic", which is the only case where the wrapper adds no filter —
    /// it still wraps, so that what the registry holds is the same type either way.
    #[must_use]
    pub fn new(inner: Arc<dyn EventSubscriber>, topics: Option<TopicScope>) -> Self {
        Self {
            inner,
            topics: topics.map(|t| t.as_slice().to_vec()).unwrap_or_default(),
        }
    }
}

impl fmt::Debug for ScopedSubscriber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedSubscriber")
            .field("subscriber", &self.inner.name())
            .field("topics", &self.topics)
            .finish()
    }
}

#[async_trait]
impl EventSubscriber for ScopedSubscriber {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn topics(&self) -> Vec<String> {
        self.topics.clone()
    }

    async fn on_event(&self, envelope: &EventEnvelope) {
        self.inner.on_event(envelope).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(prefixes: &[&str]) -> TopicScope {
        TopicScope::new(prefixes.iter().map(|p| (*p).to_string())).expect("a valid scope")
    }

    fn grant(scopes: Vec<Option<TopicScope>>) -> Grant {
        Grant {
            effective: PermissionSet::new(scopes.into_iter().map(Permission::EventsSubscribe)),
            denied: Vec::new(),
        }
    }

    fn wanted(prefixes: &[&str]) -> Vec<String> {
        prefixes.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn an_unscoped_grant_and_no_preference_is_everything() {
        assert_eq!(effective_topics(&grant(vec![None]), &[]).unwrap(), None);
    }

    #[test]
    fn a_scoped_grant_and_no_preference_is_the_grant() {
        // The line that turns "empty means everything" into "empty means what you were
        // granted", for free, out of the existing meet.
        let got = effective_topics(&grant(vec![Some(scope(&["tool.", "plugin."]))]), &[]).unwrap();
        assert_eq!(got.unwrap().as_slice(), ["plugin.", "tool."]);
    }

    #[test]
    fn the_longer_prefix_wins() {
        let got = effective_topics(
            &grant(vec![Some(scope(&["tool."]))]),
            &wanted(&["tool.execute."]),
        )
        .unwrap();
        assert_eq!(got.unwrap().as_slice(), ["tool.execute."]);
    }

    #[test]
    fn two_subscribe_grants_join_rather_than_the_first_winning() {
        let both = grant(vec![Some(scope(&["tool."])), Some(scope(&["plugin."]))]);
        let got = effective_topics(&both, &[]).unwrap();
        assert_eq!(got.unwrap().as_slice(), ["plugin.", "tool."]);
    }

    #[test]
    fn an_unscoped_grant_absorbs_a_scoped_one() {
        let both = grant(vec![None, Some(scope(&["tool."]))]);
        assert_eq!(effective_topics(&both, &[]).unwrap(), None);
    }

    #[test]
    fn a_request_outside_the_grant_is_refused() {
        let error = effective_topics(&grant(vec![Some(scope(&["tool."]))]), &wanted(&["job."]))
            .unwrap_err();
        assert!(error.message().contains("job."), "{error}");
        assert!(error.message().contains("tool."), "{error}");
    }

    #[test]
    fn a_topics_list_with_an_empty_prefix_is_refused_not_widened() {
        // `["tool.", ""]` is a mistake, and the tempting `.ok()` folds it into the same
        // `None` as "no preference" -- handing this subscriber the *whole* grant, which is
        // wider than the thing it tried to write.
        let error = effective_topics(&grant(vec![None]), &wanted(&["tool.", ""])).unwrap_err();
        assert_eq!(error.kind(), rivet_core::error::ErrorKind::InvalidArgument);
    }

    #[test]
    fn a_manifest_that_never_asked_and_a_profile_that_removed_it_say_different_things() {
        let never_asked = Grant::default();
        let removed = Grant {
            effective: PermissionSet::empty(),
            denied: vec![Permission::EventsSubscribe(None)],
        };

        let a = effective_topics(&never_asked, &[]).unwrap_err();
        let b = effective_topics(&removed, &[]).unwrap_err();

        assert!(a.message().contains("manifest"), "{a}");
        assert!(b.message().contains("profile"), "{b}");
        assert_ne!(a.message(), b.message());
    }

    #[test]
    fn a_default_grant_refuses_rather_than_allows() {
        // `GuardedRegistry::new` gained a parameter, so an embedder can forget it. The
        // direction is what matters: `Grant::default()` denies every subscriber.
        assert!(effective_topics(&Grant::default(), &[]).is_err());
    }
}
