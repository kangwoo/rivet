//! Capability versioning and the permission vocabulary.
//!
//! A "capability" is one extension slot: model, tool, context provider, policy, sandbox,
//! session store, memory, workflow, scheduler, evaluator. Each has a semver-ish version
//! so that a plugin loaded over RPC or WASM can be rejected *before* it registers
//! anything, rather than failing mid-run.

use std::fmt;

use serde::{Deserialize, Serialize};

/// `major.minor` version of a contract. Patch is intentionally absent: a contract has no
/// bug-fix-only changes, only additive (minor) and breaking (major) ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CapabilityVersion {
    pub major: u16,
    pub minor: u16,
}

impl CapabilityVersion {
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Whether a host at `self` can load a plugin built against `plugin`.
    ///
    /// Same major, and the host must be at least as new as the plugin — a plugin built
    /// against 0.3 needs fields the host only learned about in 0.3.
    ///
    /// Major `0` is treated as unstable: every minor bump is breaking, matching Cargo.
    #[must_use]
    pub const fn accepts(self, plugin: Self) -> bool {
        if self.major != plugin.major {
            return false;
        }
        if self.major == 0 {
            return self.minor == plugin.minor;
        }
        self.minor >= plugin.minor
    }
}

impl fmt::Display for CapabilityVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The kinds of extension slot a plugin can fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    Model,
    Tool,
    ContextProvider,
    Policy,
    Sandbox,
    SessionStore,
    Memory,
    Workflow,
    Scheduler,
    Evaluator,
    EventSubscriber,
    Command,
}

impl CapabilityKind {
    /// The contract version this build of `rivet-core` implements for the slot.
    #[must_use]
    pub const fn version(self) -> CapabilityVersion {
        // Every slot currently ships as part of the 0.1 contract surface. They are listed
        // individually so a single slot can advance without bumping the others.
        CapabilityVersion::new(0, 1)
    }
}

/// A permission a plugin may request in its manifest and a sandbox may enforce.
///
/// The permission set is a *closed vocabulary* on purpose. If a plugin needs something
/// not expressible here, that is a signal to extend the contract deliberately, not to add
/// an escape hatch.
/// A set of exact strings — hosts, secret keys — canonical by construction.
///
/// Scope lists are sets. Carried as a bare `Vec` they are not: two orderings of one set
/// are two values, `==` separates them, and every comparison in the codebase has to
/// remember to normalise. It did not — three times, in three consecutive commits, in
/// `allows`, in `plugin show`'s granted branch, and in its denied branch. So the
/// normalisation moved to the only place that cannot be forgotten.
///
/// There is no way to build a non-canonical one, including through `Deserialize`, which
/// goes through [`StringSet::new`] like every other caller.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct StringSet(Vec<String>);

impl StringSet {
    /// Sorted and deduplicated.
    ///
    /// # Errors
    /// [`crate::error::ErrorKind::InvalidArgument`] for an empty set or an empty member.
    /// An empty allowlist grants nothing, which is never what a manifest means; it should
    /// leave the scope out instead.
    pub fn new(items: impl IntoIterator<Item = String>) -> crate::Result<Self> {
        let mut out: Vec<String> = items.into_iter().collect();
        out.sort();
        out.dedup();
        if out.is_empty() {
            return Err(crate::Error::invalid_argument(
                "an empty scope list grants nothing; leave the scope out to ask for all",
            ));
        }
        if out.iter().any(String::is_empty) {
            return Err(crate::Error::invalid_argument(
                "a scope list has an empty entry",
            ));
        }
        Ok(Self(out))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// Already sorted and deduplicated, and known non-empty.
    fn from_canonical(items: Vec<String>) -> Option<Self> {
        (!items.is_empty()).then_some(Self(items))
    }
}

impl<'de> Deserialize<'de> for StringSet {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(Vec::<String>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// A set of topic **prefixes**, canonical by construction.
///
/// As [`StringSet`], plus absorption: an entry another entry already covers admits no
/// topic of its own, so `["tool.", "tool.execute."]` and `["tool."]` are one set and are
/// stored as one value. Without that, `allows` — which asks whether the meet *equals* what
/// was wanted — refused a grant strictly wider than the request.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TopicScope(Vec<String>);

impl TopicScope {
    /// Sorted, deduplicated, and stripped of any prefix another entry covers.
    ///
    /// # Errors
    /// [`crate::error::ErrorKind::InvalidArgument`] for an empty set or an empty prefix.
    /// An empty prefix matches every topic — it would read as narrow and behave as "all" —
    /// and an empty list means opposite things at either end: no overlap to the meet,
    /// every topic to [`crate::event::topic_matches`].
    pub fn new(prefixes: impl IntoIterator<Item = String>) -> crate::Result<Self> {
        let mut sorted: Vec<String> = prefixes.into_iter().collect();
        sorted.sort();
        sorted.dedup();
        if sorted.is_empty() {
            return Err(crate::Error::invalid_argument(
                "an empty topic scope matches every topic, which is the opposite of what \
                 an empty list means elsewhere; leave the scope out to ask for all",
            ));
        }
        if sorted.iter().any(String::is_empty) {
            return Err(crate::Error::invalid_argument(
                "a topic scope has an empty prefix, which matches every topic",
            ));
        }
        Ok(Self(Self::absorb(&sorted)))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// Drop every entry a kept entry is already a prefix of.
    ///
    /// Sorted order puts a prefix immediately before everything it covers, so comparing
    /// against the last kept entry is enough.
    fn absorb(sorted: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(sorted.len());
        for prefix in sorted {
            if out
                .last()
                .is_none_or(|kept| !prefix.starts_with(kept.as_str()))
            {
                out.push(prefix.clone());
            }
        }
        out
    }

    fn from_unsorted(mut items: Vec<String>) -> Option<Self> {
        items.sort();
        items.dedup();
        let absorbed = Self::absorb(&items);
        (!absorbed.is_empty()).then_some(Self(absorbed))
    }
}

impl<'de> Deserialize<'de> for TopicScope {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(Vec::<String>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "permission", content = "scope")]
pub enum Permission {
    /// Read files. Scope is a [`FsScope`].
    FsRead(FsScope),
    /// Write files. Scope is a [`FsScope`].
    FsWrite(FsScope),
    /// Spawn processes.
    ProcessSpawn,
    /// Outbound HTTP. `None` means any host; otherwise an allowlist of host patterns.
    NetworkHttp(Option<StringSet>),
    /// Read the session event log.
    SessionRead,
    /// Append to the session event log.
    SessionWrite,
    /// Subscribe to the event bus. `None` means every topic; otherwise an allowlist of
    /// topic **prefixes**, matched the way [`crate::event::topic_matches`] matches them.
    ///
    /// Scoped because [`crate::event::EventSubscriber::topics`] is the subscriber's own
    /// preference — it defaults to "everything" and nothing checks it. Without a scope
    /// here the grant is all-or-nothing, and `agent.text` carries the model's output.
    EventsSubscribe(Option<TopicScope>),
    /// Publish onto the event bus.
    EventsPublish,
    /// Read named secrets, by key.
    SecretsRead(StringSet),
    /// Read and write the job graph.
    JobManage,
}

/// Where filesystem access is allowed.
///
/// Ordered by reach: `Subtree ⊑ Workspace ⊑ Anywhere`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsScope {
    /// A specific subtree, relative to the workspace root.
    Subtree(String),
    /// Anywhere under the active workspace root, minus explicit denies.
    Workspace,
    /// Anywhere on the host. Requires explicit operator opt-in; never granted by default.
    Anywhere,
}

impl FsScope {
    /// A subtree scope, rejecting anything that would escape the workspace.
    ///
    /// # Errors
    /// A subtree containing `..`, an absolute path, or a Windows drive prefix is refused.
    /// Without this check `Subtree("../../../etc")` would meet `Workspace` to *itself* —
    /// the narrower-scope rule would hand a plugin access outside the workspace, which is
    /// worse than the exact-match behavior it replaced.
    pub fn subtree(path: impl Into<String>) -> crate::Result<Self> {
        let path = path.into();
        if is_contained(&path) {
            Ok(Self::Subtree(path))
        } else {
            Err(crate::Error::invalid_argument(format!(
                "subtree scope `{path}` must be a relative path inside the workspace"
            )))
        }
    }

    /// Whether this scope is safe to grant.
    ///
    /// Checked in [`FsScope::meet`] as well as at construction, because `Subtree` can also
    /// arrive by deserializing a manifest that never went through the constructor.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Subtree(p) => is_contained(p),
            Self::Workspace | Self::Anywhere => true,
        }
    }

    /// The narrower of two scopes, or `None` when neither contains the other.
    ///
    /// Two different subtrees have no meet we can express without path arithmetic, so
    /// they yield `None` — the conservative answer. An invalid subtree also yields `None`,
    /// so a manifest cannot smuggle one past the profile.
    #[must_use]
    // Spelled out one relation at a time: this is a security lattice, and a merged
    // pattern hides which direction each rule widens.
    #[allow(clippy::match_same_arms)]
    pub fn meet(&self, other: &Self) -> Option<Self> {
        if !self.is_valid() || !other.is_valid() {
            return None;
        }
        match (self, other) {
            (Self::Anywhere, o) | (o, Self::Anywhere) => Some(o.clone()),
            (Self::Workspace, o) | (o, Self::Workspace) => Some(o.clone()),
            (Self::Subtree(a), Self::Subtree(b)) if a == b => Some(Self::Subtree(a.clone())),
            (Self::Subtree(_), Self::Subtree(_)) => None,
        }
    }
}

impl Permission {
    /// The most permission both grants allow, or `None` when they overlap in nothing.
    ///
    /// This is what makes [`PermissionSet::intersect`] a real meet rather than an exact
    /// string match. Without it a plugin is *punished for declaring narrowly*: a manifest
    /// asking for `FsRead(Subtree("docs"))` against a profile granting
    /// `FsRead(Workspace)` would intersect to nothing, and the careful plugin would end
    /// up with fewer permissions than a sloppy one.
    #[must_use]
    pub fn meet(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::FsRead(a), Self::FsRead(b)) => a.meet(b).map(Self::FsRead),
            (Self::FsWrite(a), Self::FsWrite(b)) => a.meet(b).map(Self::FsWrite),
            (Self::NetworkHttp(a), Self::NetworkHttp(b)) => match (a, b) {
                (None, None) => Some(Self::NetworkHttp(None)),
                (None, Some(list)) | (Some(list), None) => {
                    Some(Self::NetworkHttp(Some(list.clone())))
                }
                (Some(x), Some(y)) => {
                    StringSet::from_canonical(intersect_sorted(x.as_slice(), y.as_slice()))
                        .map(|hosts| Self::NetworkHttp(Some(hosts)))
                }
            },
            (Self::EventsSubscribe(a), Self::EventsSubscribe(b)) => match (a, b) {
                (None, None) => Some(Self::EventsSubscribe(None)),
                (None, Some(list)) | (Some(list), None) => {
                    Some(Self::EventsSubscribe(Some(list.clone())))
                }
                (Some(x), Some(y)) => {
                    TopicScope::from_unsorted(meet_prefixes(x.as_slice(), y.as_slice()))
                        .map(|topics| Self::EventsSubscribe(Some(topics)))
                }
            },
            (Self::SecretsRead(a), Self::SecretsRead(b)) => {
                StringSet::from_canonical(intersect_sorted(a.as_slice(), b.as_slice()))
                    .map(Self::SecretsRead)
            }
            // The remaining variants carry no scope: they meet only with themselves.
            (a, b) if a == b => Some(a.clone()),
            _ => None,
        }
    }
}

/// Whether a subtree string stays inside the workspace.
///
/// Purely lexical, like [`crate::workspace::Workspace::resolve`]: `rivet-core` does no
/// I/O. Symlink resolution is the runtime's job.
fn is_contained(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        // A Windows drive prefix such as `C:`.
        && path.chars().nth(1).is_none_or(|c| c != ':')
        && !path
            .split(['/', '\\'])
            .any(|segment| segment == ".." || segment.is_empty())
}

/// Every prefix admitted by both sides, before absorption.
///
/// Two prefixes admit a common topic only when one is a prefix of the other, and then the
/// longer one is exactly the set both allow: `tool.` with `tool.execute.` gives
/// `tool.execute.`, while `tool.` with `run.` gives nothing. This is *not* the string
/// intersection [`intersect_sorted`] computes for exact-match hosts and keys.
fn meet_prefixes(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for p in a {
        for q in b {
            if q.starts_with(p.as_str()) {
                out.push(q.clone());
            } else if p.starts_with(q.as_str()) {
                out.push(p.clone());
            }
        }
    }
    out
}

fn intersect_sorted(a: &[String], b: &[String]) -> Vec<String> {
    let mut out: Vec<String> = a.iter().filter(|k| b.contains(k)).cloned().collect();
    out.sort();
    out.dedup();
    out
}

/// The set of permissions actually granted to a running unit of work.
///
/// Grants are computed by the runtime from `manifest ∩ profile ∩ session overrides`. A
/// plugin can never widen its own grant.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSet {
    granted: Vec<Permission>,
}

impl PermissionSet {
    #[must_use]
    pub fn new(granted: impl IntoIterator<Item = Permission>) -> Self {
        let mut granted: Vec<_> = granted.into_iter().collect();
        granted.sort();
        granted.dedup();
        Self { granted }
    }

    /// The empty grant: read nothing, write nothing, spawn nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn granted(&self) -> &[Permission] {
        &self.granted
    }

    /// Exact-match containment.
    ///
    /// Use this only when you know the exact permission you hold. To ask "am I allowed to
    /// do X", use [`PermissionSet::allows`], which respects scope ordering.
    #[must_use]
    pub fn contains(&self, permission: &Permission) -> bool {
        self.granted.contains(permission)
    }

    /// Whether the grant permits `wanted`, honoring scope ordering.
    ///
    /// `FsRead(Workspace)` allows `FsRead(Subtree("docs"))`, but not the reverse.
    #[must_use]
    pub fn allows(&self, wanted: &Permission) -> bool {
        self.granted
            .iter()
            .any(|held| held.meet(wanted).as_ref() == Some(wanted))
    }

    /// Meet two grants. Used to narrow a plugin's manifest request by the active profile,
    /// which is how a `readonly` profile disarms a write-capable plugin.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        let mut out = Vec::new();
        for mine in &self.granted {
            for theirs in &other.granted {
                if let Some(met) = mine.meet(theirs) {
                    out.push(met);
                }
            }
        }
        Self::new(out)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topics(list: &[&str]) -> TopicScope {
        TopicScope::new(list.iter().map(|s| (*s).to_string())).expect("valid topic scope")
    }

    fn strings(list: &[&str]) -> StringSet {
        StringSet::new(list.iter().map(|s| (*s).to_string())).expect("valid string set")
    }

    /// The property that used to live in three comparisons now lives in one constructor.
    ///
    /// `allows` asks whether the meet *equals* what was wanted, and `plugin show` compares
    /// against `effective` and `denied`. Each needed the operands normalised, each was
    /// fixed separately, and the third fix broke the second. None of them normalises
    /// anything now, because a non-canonical scope cannot be built.
    #[test]
    fn a_scope_list_is_canonical_at_construction() {
        // Order, duplicates: one set, one value.
        let a = StringSet::new(["b".to_string(), "a".to_string(), "b".to_string()]).unwrap();
        let b = StringSet::new(["a".to_string(), "b".to_string()]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.as_slice(), ["a", "b"]);

        // Prefixes additionally absorb: these three spell the same set of topics.
        let wide = TopicScope::new(["tool.".to_string()]).unwrap();
        for spelling in [
            vec!["tool.".to_string(), "tool.execute.".to_string()],
            vec!["tool.execute.".to_string(), "tool.".to_string()],
            vec!["tool.".to_string(), "tool.execute.started".to_string()],
        ] {
            assert_eq!(TopicScope::new(spelling).unwrap(), wide);
        }

        // And so the comparison that needed normalising three times does not.
        let held = PermissionSet::new(vec![Permission::EventsSubscribe(Some(
            TopicScope::new(["tool.".to_string(), "tool.execute.".to_string()]).unwrap(),
        ))]);
        assert!(held.allows(&Permission::EventsSubscribe(Some(wide))));
    }

    /// The two values that meant opposite things at either end are unconstructible.
    #[test]
    fn an_empty_or_blank_scope_cannot_be_built() {
        // Empty list: "no overlap" to the meet, "every topic" to `topic_matches`.
        assert!(TopicScope::new(Vec::new()).is_err());
        assert!(StringSet::new(Vec::new()).is_err());
        // Empty entry: reads as narrow, matches everything.
        assert!(TopicScope::new([String::new()]).is_err());
        assert!(StringSet::new([String::new()]).is_err());

        // Including through `Deserialize`, which is the door Phase 6 comes in by.
        for wire in ["[]", r#"[""]"#, r#"["tool.", ""]"#] {
            assert!(
                serde_json::from_str::<TopicScope>(wire).is_err(),
                "`{wire}` must not deserialize"
            );
        }
        assert!(serde_json::from_str::<TopicScope>(r#"["tool."]"#).is_ok());
    }

    /// Topic scopes are prefixes, so their meet is not the set intersection hosts get.
    #[test]
    fn topic_scopes_meet_on_the_narrower_prefix() {
        let all = Permission::EventsSubscribe(None);
        let tools = Permission::EventsSubscribe(Some(topics(&["tool."])));
        let executes = Permission::EventsSubscribe(Some(topics(&["tool.execute."])));
        let runs = Permission::EventsSubscribe(Some(topics(&["run."])));

        // Unrestricted meets a list to that list, in either order.
        assert_eq!(all.meet(&tools), Some(tools.clone()));
        assert_eq!(tools.meet(&all), Some(tools.clone()));

        // The longer prefix is exactly the set both admit -- a string intersection would
        // have dropped it and silently widened the grant to `tool.`.
        assert_eq!(tools.meet(&executes), Some(executes.clone()));
        assert_eq!(executes.meet(&tools), Some(executes));

        // Prefixes that admit no common topic overlap in nothing.
        assert_eq!(tools.meet(&runs), None);
    }

    /// The property the `reviewer` profile is granted for: `agent.text` must not be
    /// reachable through a grant that names its siblings.
    #[test]
    fn a_sibling_prefix_does_not_admit_agent_text() {
        let narrowed = Permission::EventsSubscribe(Some(topics(&[
            "agent.request.",
            "agent.run.",
            "agent.turn.",
        ])));
        let wants_text = Permission::EventsSubscribe(Some(topics(&["agent.text."])));
        assert_eq!(narrowed.meet(&wants_text), None);

        let set = PermissionSet::new(vec![narrowed]);
        assert!(!set.allows(&wants_text));
        assert!(set.allows(&Permission::EventsSubscribe(Some(topics(&[
            "agent.run.completed"
        ])))));
    }

    #[test]
    fn zero_major_treats_every_minor_as_breaking() {
        let host = CapabilityVersion::new(0, 2);
        assert!(host.accepts(CapabilityVersion::new(0, 2)));
        assert!(!host.accepts(CapabilityVersion::new(0, 1)));
        assert!(!host.accepts(CapabilityVersion::new(0, 3)));
    }

    #[test]
    fn stable_major_accepts_older_minors_only() {
        let host = CapabilityVersion::new(1, 4);
        assert!(host.accepts(CapabilityVersion::new(1, 4)));
        assert!(host.accepts(CapabilityVersion::new(1, 0)));
        assert!(!host.accepts(CapabilityVersion::new(1, 5)), "host too old");
        assert!(
            !host.accepts(CapabilityVersion::new(2, 0)),
            "major mismatch"
        );
    }

    #[test]
    fn intersection_narrows_a_plugin_to_the_profile() {
        let plugin = PermissionSet::new([
            Permission::FsRead(FsScope::Workspace),
            Permission::FsWrite(FsScope::Workspace),
            Permission::ProcessSpawn,
        ]);
        let readonly_profile = PermissionSet::new([
            Permission::FsRead(FsScope::Workspace),
            Permission::SessionRead,
        ]);
        let effective = plugin.intersect(&readonly_profile);
        assert!(effective.contains(&Permission::FsRead(FsScope::Workspace)));
        assert!(!effective.contains(&Permission::FsWrite(FsScope::Workspace)));
        assert!(!effective.contains(&Permission::ProcessSpawn));
        assert_eq!(effective.granted().len(), 1);
    }

    #[test]
    fn declaring_narrowly_is_not_punished() {
        // A careful plugin asks for exactly what it needs; a sloppy one asks for the
        // whole workspace. The careful one must not end up with *less*.
        let careful = PermissionSet::new([Permission::FsRead(FsScope::Subtree("docs".into()))]);
        let profile = PermissionSet::new([Permission::FsRead(FsScope::Workspace)]);
        let effective = careful.intersect(&profile);
        assert_eq!(
            effective.granted(),
            [Permission::FsRead(FsScope::Subtree("docs".into()))],
            "the narrower scope survives the meet"
        );
    }

    #[test]
    fn the_profile_caps_a_greedy_plugin() {
        let greedy = PermissionSet::new([Permission::FsWrite(FsScope::Anywhere)]);
        let profile = PermissionSet::new([Permission::FsWrite(FsScope::Workspace)]);
        assert_eq!(
            greedy.intersect(&profile).granted(),
            [Permission::FsWrite(FsScope::Workspace)],
            "a plugin cannot widen past the profile"
        );
    }

    #[test]
    fn an_escaping_subtree_grants_nothing() {
        // The dangerous case: a manifest asks for a subtree that climbs out of the
        // workspace. Under a naive "narrower wins" rule this would survive the meet with
        // a `Workspace` profile and hand the plugin access to /etc.
        let escaping =
            PermissionSet::new([Permission::FsWrite(FsScope::Subtree("../../../etc".into()))]);
        let profile = PermissionSet::new([Permission::FsWrite(FsScope::Workspace)]);
        assert!(
            escaping.intersect(&profile).is_empty(),
            "an escaping subtree must not survive the meet"
        );
        assert!(!profile.allows(&Permission::FsWrite(FsScope::Subtree("../etc".into()))));
    }

    #[test]
    fn the_subtree_constructor_rejects_escapes() {
        assert!(FsScope::subtree("docs").is_ok());
        assert!(FsScope::subtree("docs/api").is_ok());
        assert!(FsScope::subtree("../etc").is_err());
        assert!(FsScope::subtree("docs/../../etc").is_err());
        assert!(FsScope::subtree("/etc").is_err());
        assert!(FsScope::subtree("C:/Windows").is_err());
        assert!(FsScope::subtree("").is_err());
    }

    #[test]
    fn disjoint_subtrees_meet_to_nothing() {
        let a = PermissionSet::new([Permission::FsRead(FsScope::Subtree("docs".into()))]);
        let b = PermissionSet::new([Permission::FsRead(FsScope::Subtree("src".into()))]);
        assert!(a.intersect(&b).is_empty());
    }

    #[test]
    fn network_allowlists_intersect() {
        let plugin = PermissionSet::new([Permission::NetworkHttp(Some(strings(&[
            "api.openai.com",
            "evil.test",
        ])))]);
        let profile = PermissionSet::new([Permission::NetworkHttp(Some(strings(&[
            "api.openai.com",
            "api.anthropic.com",
        ])))]);
        assert_eq!(
            plugin.intersect(&profile).granted(),
            [Permission::NetworkHttp(Some(strings(&["api.openai.com"])))]
        );
    }

    #[test]
    fn any_host_is_capped_by_an_allowlist() {
        let plugin = PermissionSet::new([Permission::NetworkHttp(None)]);
        let profile =
            PermissionSet::new([Permission::NetworkHttp(Some(strings(&["api.openai.com"])))]);
        assert_eq!(
            plugin.intersect(&profile).granted(),
            [Permission::NetworkHttp(Some(strings(&["api.openai.com"])))],
            "`any host` must not survive a profile that names hosts"
        );
    }

    #[test]
    fn secret_keys_intersect() {
        let plugin = PermissionSet::new([Permission::SecretsRead(strings(&["OPENAI_API_KEY"]))]);
        let profile = PermissionSet::new([Permission::SecretsRead(strings(&[
            "OPENAI_API_KEY",
            "OTHER",
        ]))]);
        assert_eq!(
            plugin.intersect(&profile).granted(),
            [Permission::SecretsRead(strings(&["OPENAI_API_KEY"]))]
        );
    }

    #[test]
    fn allows_respects_scope_ordering() {
        let held = PermissionSet::new([Permission::FsRead(FsScope::Workspace)]);
        assert!(held.allows(&Permission::FsRead(FsScope::Subtree("docs".into()))));
        assert!(held.allows(&Permission::FsRead(FsScope::Workspace)));
        assert!(
            !held.allows(&Permission::FsRead(FsScope::Anywhere)),
            "workspace access must not imply host-wide access"
        );
        assert!(!held.allows(&Permission::FsWrite(FsScope::Workspace)));
    }

    #[test]
    fn empty_grant_permits_nothing() {
        let empty = PermissionSet::empty();
        assert!(empty.is_empty());
        assert!(!empty.contains(&Permission::ProcessSpawn));
        assert!(!empty.allows(&Permission::ProcessSpawn));
    }
}
