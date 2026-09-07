//! `rivet.telemetry-log` — the bus, as structured `tracing` records.
//!
//! The first plugin that only *watches*. It registers one [`EventSubscriber`] and does
//! nothing else: no file, no socket, no wait. That is deliberate on two counts.
//!
//! **It writes through `tracing`, not to a file.** A `fs_write` grant is held only by
//! `developer` and `ci`, so a plugin that wrote its own log would refuse to load under
//! three of the five shipped profiles — and telemetry that disappears when you narrow the
//! profile is worse than no telemetry. Emitting `tracing` events makes the destination the
//! operator's choice (`RUST_LOG`, `RIVET_LOG_FORMAT=json`, a redirect) and needs no
//! permission at all.
//!
//! **`load` returns immediately.** `docs/architecture.md` §11-15 records that the loader
//! puts no deadline on `Plugin::load`, so a plugin that waits inside it for something
//! unreachable hangs the host with no message and no `FAILED` record. This one has nothing
//! to wait for. It adds a surface without adding that risk.
//!
//! # Configured versus default
//!
//! The distinction the whole plugin turns on. The default topic list is a *preference*: if
//! the profile's grant is narrower, it subscribes to the narrower thing and says nothing.
//! Anything the operator **wrote down** is a *promise*: if the grant removes part of it,
//! `load` fails and names what went missing.
//!
//! That asymmetry is not tidiness. Topic prefixes meet as a union — `meet_prefixes` pushes
//! a result per matching pair — so a `readonly` profile against `topics = [… ,
//! "agent.text."]` produces a *non-empty* meet with only `agent.text.` gone. The host's
//! guard passes it, because the guard's job is to refuse an empty meet. Nobody but this
//! plugin is in a position to notice that the operator asked for something they did not
//! get, and telemetry that quietly under-reports is the exact failure this phase exists to
//! remove.

mod record;

use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::capability::{Permission, TopicScope};
use rivet_core::error::Error;
use rivet_core::event::{EventEnvelope, EventSubscriber};
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};
use serde_json::Value;

pub use record::Level;

/// The plugin id this crate registers under.
pub const PLUGIN_ID: &str = "rivet.telemetry-log";

/// This crate's `rivet-plugin.toml`, for a host catalog to hand to the loader.
pub const MANIFEST_TOML: &str = include_str!("../rivet-plugin.toml");

/// The name the subscriber registers under, and the one a lag report will show.
pub const SUBSCRIBER_NAME: &str = "telemetry.log";

/// What this plugin subscribes to when the config says nothing.
///
/// `agent.text.` is **absent**, and it is enumerated rather than excluded because prefixes
/// cannot express "not". The cost is the same one `Profile::subscribable_topics` pays: a
/// new topic under `agent.` is not logged until somebody adds it here. It fails closed.
///
/// These six are inside every shipped profile's grant, so an unconfigured telemetry plugin
/// loads everywhere (`the_default_topics_survive_every_shipped_profile`).
pub const DEFAULT_TOPICS: [&str; 6] = [
    "agent.run.",
    "agent.turn.",
    "agent.request.",
    "tool.",
    "plugin.",
    "runtime.",
];

/// The prefix `include_conversation` adds.
pub const CONVERSATION_TOPIC: &str = "agent.text.";

/// Emits one `tracing` event per bus event.
#[derive(Clone, Debug)]
pub struct TelemetryLogPlugin {
    manifest: PluginManifest,
}

impl TelemetryLogPlugin {
    #[must_use]
    pub fn new(manifest: PluginManifest) -> Self {
        Self { manifest }
    }
}

/// `[plugins."rivet.telemetry-log"]`, resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The prefixes to subscribe to.
    pub topics: Vec<String>,
    /// The subset of `topics` the operator **wrote down**.
    ///
    /// The whole asymmetry lives in this field. A prefix in here is a promise and its loss
    /// to a narrowing grant is an error; a prefix only in `topics` came from
    /// [`DEFAULT_TOPICS`], is a preference, and bends. `include_conversation = true` puts
    /// [`CONVERSATION_TOPIC`] in here for the same reason a written `topics` list goes in
    /// whole: it is an instruction, not a default.
    pub promised: Vec<String>,
    pub level: Level,
    /// Whether the operator explicitly asked for the model's output.
    pub include_conversation: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            topics: DEFAULT_TOPICS.iter().map(|t| (*t).to_string()).collect(),
            promised: Vec::new(),
            level: Level::Info,
            include_conversation: false,
        }
    }
}

impl Settings {
    /// Read `[plugins."rivet.telemetry-log"]`.
    ///
    /// # Errors
    /// A `level` that is not one of the four, or a `topics` entry that is not a string.
    /// Both are startup errors for the same reason a bad profile name is: a typo that
    /// quietly becomes a default is the failure mode worth being strict about.
    pub fn from_config(config: &Value) -> rivet_core::Result<Self> {
        let mut settings = Self::default();

        if let Some(level) = config.get("level") {
            let name = level.as_str().ok_or_else(|| {
                Error::invalid_argument("`level` must be a string: trace, debug, info or warn")
            })?;
            settings.level = Level::parse(name)?;
        }

        if let Some(flag) = config.get("include_conversation") {
            settings.include_conversation = flag.as_bool().ok_or_else(|| {
                Error::invalid_argument("`include_conversation` must be true or false")
            })?;
        }

        if let Some(topics) = config.get("topics") {
            let list = topics
                .as_array()
                .ok_or_else(|| Error::invalid_argument("`topics` must be a list of prefixes"))?;
            let mut prefixes = Vec::with_capacity(list.len());
            for entry in list {
                let prefix = entry.as_str().ok_or_else(|| {
                    Error::invalid_argument("every entry in `topics` must be a string")
                })?;
                prefixes.push(prefix.to_string());
            }
            if prefixes.is_empty() {
                return Err(Error::invalid_argument(
                    "`topics = []` subscribes to nothing; remove the key to take the default",
                ));
            }
            settings.promised.clone_from(&prefixes);
            settings.topics = prefixes;
        }

        // `include_conversation = false` does not merely drop the records: the prefix is
        // never subscribed to. Counting the deltas would mean receiving them, and
        // receiving them means they exist somewhere.
        if settings.include_conversation {
            if !settings.topics.iter().any(|t| t == CONVERSATION_TOPIC) {
                settings.topics.push(CONVERSATION_TOPIC.to_string());
            }
            if !settings.promised.iter().any(|t| t == CONVERSATION_TOPIC) {
                settings.promised.push(CONVERSATION_TOPIC.to_string());
            }
        }

        Ok(settings)
    }

    /// Which **promised** prefixes the grant does not carry whole.
    ///
    /// Empty means every instruction the operator wrote survived. A non-empty result is
    /// always an error; the defaults are filtered out before this returns, because they
    /// were a preference.
    #[must_use]
    pub fn broken_promises(&self, grant: &[Permission]) -> Vec<String> {
        self.narrowed_by(grant)
            .into_iter()
            .filter(|missing| self.promised.contains(missing))
            .collect()
    }

    /// Which of `self.topics` the grant does not carry **whole**, promised or not.
    ///
    /// "Whole" is the load-bearing word, and it is not the same as "overlaps at all". A
    /// prefix `p` survives the meet intact only if the grant holds a prefix `p` itself
    /// starts with — anything *narrower* than `p` keeps a slice of it and drops the rest.
    ///
    /// The case that makes the difference concrete: `topics = ["agent."]` against a
    /// narrowed profile, whose grant is `agent.request.`, `agent.run.`, `agent.turn.` and
    /// four others. `meet_prefixes` unions, so the meet is those three — non-empty, so the
    /// host's guard passes it, and `agent.text.` is gone without a word. An overlap test
    /// calls that survival, because `agent.request.` does start with `agent.`; the module
    /// doc's promise ("`load` fails and names what went missing") would then be false for
    /// the one input it was written for.
    #[must_use]
    pub fn narrowed_by(&self, grant: &[Permission]) -> Vec<String> {
        let granted = match subscribable(grant) {
            // No `events_subscribe` at all. The host's guard produces the message for
            // that, with the two sentences that tell a manifest problem from a profile
            // one, so this plugin says nothing and lets registration fail.
            Subscribable::Nothing | Subscribable::Everything => return Vec::new(),
            Subscribable::Scoped(topics) => topics,
        };
        self.topics
            .iter()
            .filter(|wanted| {
                !granted
                    .as_slice()
                    .iter()
                    .any(|held| wanted.starts_with(held.as_str()))
            })
            .cloned()
            .collect()
    }
}

/// What a grant says about subscribing.
///
/// Three cases, and collapsing any two of them loses something: "not granted at all" is
/// the host guard's error, "granted without a scope" narrows nothing, and only the third
/// can remove a prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subscribable<'a> {
    /// No `events_subscribe` in the grant.
    Nothing,
    /// Granted, unscoped: every topic.
    Everything,
    /// Granted, scoped to these prefixes.
    Scoped(&'a TopicScope),
}

fn subscribable(grant: &[Permission]) -> Subscribable<'_> {
    for permission in grant {
        if let Permission::EventsSubscribe(scope) = permission {
            return match scope {
                None => Subscribable::Everything,
                Some(topics) => Subscribable::Scoped(topics),
            };
        }
    }
    Subscribable::Nothing
}

#[async_trait]
impl Plugin for TelemetryLogPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    /// Read the config, register one subscriber, return. Nothing else.
    ///
    /// # Errors
    /// A malformed setting, or a grant that removes part of a list the operator wrote.
    async fn load(&self, ctx: PluginContext) -> rivet_core::Result<PluginHandle> {
        let settings = Settings::from_config(&ctx.config)?;
        let missing = settings.broken_promises(ctx.permissions.granted());
        if !missing.is_empty() {
            return Err(Error::plugin(format!(
                "the active profile does not grant {missing:?} in full, which \
                 `[plugins.\"{PLUGIN_ID}\"]` asked to log. Telemetry that quietly logs less \
                 than it was told to is worse than none: remove or narrow those prefixes, or \
                 run under a profile that grants them (`rivet plugin show {PLUGIN_ID}`)."
            )));
        }

        let subscriber: Arc<dyn EventSubscriber> = Arc::new(LogSubscriber {
            topics: settings.topics.clone(),
            level: settings.level,
        });
        ctx.registry.register_subscriber(subscriber).await?;
        Ok(PluginHandle::new([format!("subscriber:{SUBSCRIBER_NAME}")]))
    }

    /// Nothing to undo. The pump is the registry's, and `unregister_all` aborts it.
    async fn unload(&self, _ctx: PluginContext) -> rivet_core::Result<()> {
        Ok(())
    }
}

/// One `tracing` event per bus event, with a fixed field set.
#[derive(Debug)]
pub struct LogSubscriber {
    topics: Vec<String>,
    level: Level,
}

impl LogSubscriber {
    /// A subscriber over an explicit list, for tests and embedders.
    #[must_use]
    pub fn new(topics: Vec<String>, level: Level) -> Self {
        Self { topics, level }
    }
}

#[async_trait]
impl EventSubscriber for LogSubscriber {
    fn name(&self) -> &'static str {
        SUBSCRIBER_NAME
    }

    fn topics(&self) -> Vec<String> {
        self.topics.clone()
    }

    async fn on_event(&self, envelope: &EventEnvelope) {
        record::emit(self.level, envelope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_matches_the_crate() {
        let manifest = rivet_plugin::parse(MANIFEST_TOML).expect("the shipped manifest parses");
        assert_eq!(manifest.id.as_str(), PLUGIN_ID);
        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert!(manifest.is_compatible_with(rivet_core::ABI_VERSION));
        assert_eq!(
            manifest.capabilities,
            [rivet_core::capability::CapabilityKind::EventSubscriber]
        );
        assert_eq!(
            manifest.permissions,
            [Permission::EventsSubscribe(None)],
            "it asks for every topic and lets the profile narrow it; it asks for nothing else"
        );
    }

    #[test]
    fn the_default_topics_leave_out_the_conversation() {
        let settings = Settings::default();
        assert!(
            !settings
                .topics
                .iter()
                .any(|t| t.starts_with(CONVERSATION_TOPIC)),
            "the model's output is not logged unless somebody asks for it: {:?}",
            settings.topics
        );
        assert!(!settings.include_conversation);
    }

    #[test]
    fn include_conversation_off_means_it_does_not_even_subscribe() {
        // Not "receives and discards". Counting the deltas would mean receiving them.
        let settings = Settings::from_config(&serde_json::json!({})).unwrap();
        assert!(!settings.topics.iter().any(|t| t == CONVERSATION_TOPIC));

        let asked =
            Settings::from_config(&serde_json::json!({"include_conversation": true})).unwrap();
        assert!(asked.topics.iter().any(|t| t == CONVERSATION_TOPIC));
    }

    #[test]
    fn configured_topics_replace_the_default() {
        let settings = Settings::from_config(&serde_json::json!({"topics": ["job."]})).unwrap();
        assert_eq!(settings.topics, ["job."]);
        assert_eq!(settings.promised, ["job."], "a written list is a promise");
    }

    #[test]
    fn an_empty_configured_topic_list_is_a_configuration_error() {
        // `[]` reads as "nothing" here and as "everything" at the delivery end. Refusing it
        // is the same call `TopicScope::new` makes.
        let error = Settings::from_config(&serde_json::json!({"topics": []})).unwrap_err();
        assert_eq!(error.kind(), rivet_core::error::ErrorKind::InvalidArgument);
    }

    #[test]
    fn a_misspelled_level_is_a_configuration_error() {
        let error = Settings::from_config(&serde_json::json!({"level": "verbose"})).unwrap_err();
        assert!(error.message().contains("verbose"), "{error}");
    }

    #[test]
    fn a_narrowing_grant_names_exactly_what_it_removed() {
        let grant = [Permission::EventsSubscribe(Some(
            TopicScope::new(["tool.".to_string(), "plugin.".to_string()]).unwrap(),
        ))];
        let settings = Settings::default();
        let missing = settings.narrowed_by(&grant);
        assert!(missing.iter().any(|m| m == "agent.run."), "{missing:?}");
        assert!(!missing.iter().any(|m| m == "tool."), "{missing:?}");
        assert!(
            settings.broken_promises(&grant).is_empty(),
            "nobody wrote the defaults down, so nothing was promised"
        );
    }

    #[test]
    fn include_conversation_is_a_promise_even_without_a_topics_list() {
        // The §4.2 row that is easy to lose: `include_conversation = true` on its own is an
        // instruction, so a profile that will not grant `agent.text.` has to refuse rather
        // than log less than it was told to.
        use rivet_core::capability::TopicScope;
        let settings =
            Settings::from_config(&serde_json::json!({"include_conversation": true})).unwrap();
        let narrowed = [Permission::EventsSubscribe(Some(
            TopicScope::new(["tool.".to_string(), "agent.run.".to_string()]).unwrap(),
        ))];
        assert_eq!(settings.broken_promises(&narrowed), [CONVERSATION_TOPIC]);
    }

    #[test]
    fn a_prefix_the_grant_only_partly_covers_counts_as_narrowed() {
        // The input the overlap test got wrong. `agent.` against a narrowed profile keeps
        // `agent.request.`, `agent.run.` and `agent.turn.` and loses `agent.text.`, so the
        // meet is non-empty and the host's guard -- whose job is to refuse an *empty* meet
        // -- waves it through. If this plugin also called that survival, the module doc's
        // promise would be false for the one case it exists to catch.
        let grant = [Permission::EventsSubscribe(Some(
            TopicScope::new(
                ["agent.request.", "agent.run.", "agent.turn.", "tool."]
                    .iter()
                    .map(|t| (*t).to_string()),
            )
            .unwrap(),
        ))];
        let settings = Settings::from_config(&serde_json::json!({"topics": ["agent."]})).unwrap();
        assert_eq!(settings.narrowed_by(&grant), ["agent."]);
        assert_eq!(
            settings.broken_promises(&grant),
            ["agent."],
            "a written prefix the grant only half-carries has to fail the load"
        );
    }

    #[test]
    fn a_grant_wider_than_what_was_written_keeps_the_promise() {
        // The other direction, and the one that must *not* become an error: asking for
        // `tool.execute.` under a grant of `tool.` loses nothing.
        let grant = [Permission::EventsSubscribe(Some(
            TopicScope::new(["tool.".to_string()]).unwrap(),
        ))];
        let settings =
            Settings::from_config(&serde_json::json!({"topics": ["tool.execute."]})).unwrap();
        assert!(settings.broken_promises(&grant).is_empty());
    }

    #[test]
    fn an_unscoped_grant_narrows_nothing() {
        assert!(
            Settings::default()
                .narrowed_by(&[Permission::EventsSubscribe(None)])
                .is_empty()
        );
    }
}
