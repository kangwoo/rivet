//! Strongly typed, opaque identifiers.
//!
//! Every id is a `UUIDv7` so that ids sort by creation time — useful for session logs and
//! for debugging without a separate timestamp lookup. Ids are opaque: never parse
//! meaning out of them.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Human-readable prefix used when rendering the id.
            pub const PREFIX: &'static str = $prefix;

            /// Mint a new, time-ordered id.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wrap an existing UUID (for rehydrating from storage).
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// A short, log-friendly form. Not unique across a large corpus — display only.
            #[must_use]
            pub fn short(&self) -> String {
                let s = self.0.simple().to_string();
                format!("{}_{}", Self::PREFIX, &s[s.len() - 8..])
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}_{}", Self::PREFIX, self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                let raw = s.strip_prefix(concat!($prefix, "_")).unwrap_or(s);
                Ok(Self(Uuid::parse_str(raw)?))
            }
        }
    };
}

define_id!(
    /// A durable conversation log. Owns the append-only event stream.
    SessionId, "ses"
);
define_id!(
    /// One configured agent (model + tool scope + context providers).
    AgentId, "agt"
);
define_id!(
    /// A single execution of an agent against a session. A task may have many runs.
    RunId, "run"
);
define_id!(
    /// A unit of durable intent in the task graph.
    TaskId, "tsk"
);
define_id!(
    /// A single tool invocation requested by the model.
    ToolCallId, "tc"
);
define_id!(
    /// A loaded plugin instance.
    PluginInstanceId, "pli"
);
define_id!(
    /// A single emitted event on the bus.
    EventId, "evt"
);
define_id!(
    /// A prepared sandbox.
    SandboxId, "sbx"
);
define_id!(
    /// An approval request awaiting a human decision.
    ApprovalId, "apr"
);

/// A stable, human-authored plugin identity such as `rivet.tool-git`.
///
/// Unlike the UUID ids above this is chosen by the plugin author, appears in config
/// files, and must stay stable across versions.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PluginId(String);

impl PluginId {
    /// Create a plugin id, validating the `namespace.name` shape.
    pub fn new(raw: impl Into<String>) -> crate::Result<Self> {
        let raw = raw.into();
        let valid = !raw.is_empty()
            && raw.len() <= 128
            && raw.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_')
            })
            && raw.contains('.')
            && !raw.starts_with('.')
            && !raw.ends_with('.');
        if valid {
            Ok(Self(raw))
        } else {
            Err(crate::Error::invalid_argument(format!(
                "plugin id `{raw}` must be lowercase `namespace.name`, \
                 using only [a-z0-9._-], with at least one dot"
            )))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The namespace segment, e.g. `rivet` in `rivet.tool-git`.
    #[must_use]
    pub fn namespace(&self) -> &str {
        self.0.split('.').next().unwrap_or(&self.0)
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PluginId({})", self.0)
    }
}

impl FromStr for PluginId {
    type Err = crate::Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_display_and_parse() {
        let id = SessionId::new();
        let parsed: SessionId = id.to_string().parse().expect("parse");
        assert_eq!(id, parsed);
    }

    #[test]
    fn ids_are_time_ordered() {
        let a = TaskId::new();
        let b = TaskId::new();
        assert!(a < b, "uuidv7 ids must sort by creation time");
    }

    #[test]
    fn plugin_id_requires_namespace() {
        assert!(PluginId::new("rivet.tool-git").is_ok());
        assert!(PluginId::new("toolgit").is_err(), "missing namespace");
        assert!(PluginId::new("Rivet.Git").is_err(), "uppercase rejected");
        assert!(PluginId::new("rivet.").is_err(), "trailing dot rejected");
    }

    #[test]
    fn plugin_id_exposes_namespace() {
        let id = PluginId::new("acme.tool-jira").unwrap();
        assert_eq!(id.namespace(), "acme");
    }
}
