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
    NetworkHttp(Option<Vec<String>>),
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
    EventsSubscribe(Option<Vec<String>>),
    /// Publish onto the event bus.
    EventsPublish,
    /// Read named secrets, by key.
    SecretsRead(Vec<String>),
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
            (Self::NetworkHttp(a), Self::NetworkHttp(b)) => {
                match meet_allowlist(a.as_deref(), b.as_deref()) {
                    HostMeet::AnyHost => Some(Self::NetworkHttp(None)),
                    HostMeet::Hosts(hosts) => Some(Self::NetworkHttp(Some(hosts))),
                    HostMeet::Disjoint => None,
                }
            }
            (Self::EventsSubscribe(a), Self::EventsSubscribe(b)) => {
                match meet_topics(a.as_deref(), b.as_deref()) {
                    TopicMeet::AllTopics => Some(Self::EventsSubscribe(None)),
                    TopicMeet::Topics(topics) => Some(Self::EventsSubscribe(Some(topics))),
                    TopicMeet::Disjoint => None,
                }
            }
            (Self::SecretsRead(a), Self::SecretsRead(b)) => {
                let keys = intersect_sorted(a, b);
                if keys.is_empty() {
                    None
                } else {
                    Some(Self::SecretsRead(keys))
                }
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

/// The result of meeting two host allowlists.
///
/// Spelled as an enum rather than `Option<Option<Vec<String>>>` because the two "nothing"
/// cases mean opposite things: `AnyHost` is the widest possible grant, `Disjoint` is no
/// grant at all. Collapsing them would let an empty intersection read as "any host".
enum HostMeet {
    /// Neither side restricted hosts.
    AnyHost,
    /// The hosts both sides allow.
    Hosts(Vec<String>),
    /// The allowlists share nothing.
    Disjoint,
}

/// `None` means "any host". The meet of any-host with a list is that list; the meet of
/// two lists is their intersection, and an empty intersection means no overlap at all.
fn meet_allowlist(a: Option<&[String]>, b: Option<&[String]>) -> HostMeet {
    match (a, b) {
        (None, None) => HostMeet::AnyHost,
        (None, Some(list)) | (Some(list), None) => HostMeet::Hosts(list.to_vec()),
        (Some(x), Some(y)) => {
            let hosts = intersect_sorted(x, y);
            if hosts.is_empty() {
                HostMeet::Disjoint
            } else {
                HostMeet::Hosts(hosts)
            }
        }
    }
}

/// The result of meeting two topic scopes. Same three-way shape as [`HostMeet`], and for
/// the same reason: "no restriction" and "no overlap" are opposites.
enum TopicMeet {
    AllTopics,
    Topics(Vec<String>),
    Disjoint,
}

/// `None` means "every topic". Otherwise the operands are **prefixes**, so this is not the
/// set intersection [`meet_allowlist`] computes for hosts.
///
/// Two prefixes admit a common topic only when one is a prefix of the other, and then the
/// longer one is exactly the set of topics both allow: `tool.` met with `tool.execute.` is
/// `tool.execute.`, while `tool.` met with `run.` is nothing. Taking the string
/// intersection instead would drop `tool.execute.` on the floor and silently widen or
/// narrow depending on which side spelled what.
fn meet_topics(a: Option<&[String]>, b: Option<&[String]>) -> TopicMeet {
    match (a, b) {
        (None, None) => TopicMeet::AllTopics,
        (None, Some(list)) | (Some(list), None) => TopicMeet::Topics(list.to_vec()),
        (Some(x), Some(y)) => {
            let mut topics = Vec::new();
            for p in x {
                for q in y {
                    if q.starts_with(p.as_str()) {
                        topics.push(q.clone());
                    } else if p.starts_with(q.as_str()) {
                        topics.push(p.clone());
                    }
                }
            }
            topics.sort();
            topics.dedup();
            if topics.is_empty() {
                TopicMeet::Disjoint
            } else {
                TopicMeet::Topics(topics)
            }
        }
    }
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

    /// Topic scopes are prefixes, so their meet is not the set intersection hosts get.
    #[test]
    fn topic_scopes_meet_on_the_narrower_prefix() {
        let all = Permission::EventsSubscribe(None);
        let tools = Permission::EventsSubscribe(Some(vec!["tool.".into()]));
        let executes = Permission::EventsSubscribe(Some(vec!["tool.execute.".into()]));
        let runs = Permission::EventsSubscribe(Some(vec!["run.".into()]));

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
        let narrowed = Permission::EventsSubscribe(Some(vec![
            "agent.request.".into(),
            "agent.run.".into(),
            "agent.turn.".into(),
        ]));
        let wants_text = Permission::EventsSubscribe(Some(vec!["agent.text.".into()]));
        assert_eq!(narrowed.meet(&wants_text), None);

        let set = PermissionSet::new(vec![narrowed]);
        assert!(!set.allows(&wants_text));
        assert!(set.allows(&Permission::EventsSubscribe(Some(vec![
            "agent.run.completed".into()
        ]))));
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
        let plugin = PermissionSet::new([Permission::NetworkHttp(Some(vec![
            "api.openai.com".into(),
            "evil.test".into(),
        ]))]);
        let profile = PermissionSet::new([Permission::NetworkHttp(Some(vec![
            "api.openai.com".into(),
            "api.anthropic.com".into(),
        ]))]);
        assert_eq!(
            plugin.intersect(&profile).granted(),
            [Permission::NetworkHttp(Some(vec!["api.openai.com".into()]))]
        );
    }

    #[test]
    fn any_host_is_capped_by_an_allowlist() {
        let plugin = PermissionSet::new([Permission::NetworkHttp(None)]);
        let profile =
            PermissionSet::new([Permission::NetworkHttp(Some(vec!["api.openai.com".into()]))]);
        assert_eq!(
            plugin.intersect(&profile).granted(),
            [Permission::NetworkHttp(Some(vec!["api.openai.com".into()]))],
            "`any host` must not survive a profile that names hosts"
        );
    }

    #[test]
    fn secret_keys_intersect() {
        let plugin = PermissionSet::new([Permission::SecretsRead(vec!["OPENAI_API_KEY".into()])]);
        let profile = PermissionSet::new([Permission::SecretsRead(vec![
            "OPENAI_API_KEY".into(),
            "OTHER".into(),
        ])]);
        assert_eq!(
            plugin.intersect(&profile).granted(),
            [Permission::SecretsRead(vec!["OPENAI_API_KEY".into()])]
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
