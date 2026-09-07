//! `rivet-plugin.toml` — parsing and validation.
//!
//! The file is the manifest. Nothing about it lives in Rust literals, so a plugin's
//! `manifest()` and what `rivet plugin show` prints are the same bytes.
//!
//! Parsing is strict in both directions: an unknown key is an error (a typo must not
//! become a silently absent permission) and a scope that would escape the workspace is
//! rejected here rather than left for [`rivet_core::capability::FsScope::meet`] to drop.
//! Letting it through would load the plugin with a quietly empty grant instead of telling
//! its author the manifest is wrong.

use std::fmt;

use rivet_core::capability::{
    CapabilityKind, CapabilityVersion, FsScope, Permission, StringSet, TopicScope,
};
use rivet_core::error::Error;
use rivet_core::id::PluginId;
use rivet_core::plugin::PluginManifest;
use serde::Deserialize;

use crate::source::Origin;

/// Every permission name the manifest vocabulary accepts, for error messages.
const PERMISSION_NAMES: &str = "fs_read, fs_write, process_spawn, network_http, \
     session_read, session_write, events_subscribe, events_publish, secrets_read, \
     job_manage";

/// Parse a manifest.
///
/// # Errors
/// [`rivet_core::error::ErrorKind::Plugin`] for malformed TOML, an unknown or missing
/// field, an id that is not `namespace.name`, an `abi_version` that is not `major.minor`,
/// an empty `capabilities`, or a permission whose scope does not match its vocabulary.
pub fn parse(text: &str) -> rivet_core::Result<PluginManifest> {
    parse_inner(text, None)
}

/// Parse a manifest, naming `origin` in every error.
///
/// # Errors
/// As [`parse`].
pub fn parse_from(text: &str, origin: &Origin) -> rivet_core::Result<PluginManifest> {
    parse_inner(text, Some(origin))
}

fn parse_inner(text: &str, origin: Option<&Origin>) -> rivet_core::Result<PluginManifest> {
    let raw: RawManifest =
        toml::from_str(text).map_err(|e| bad(origin, "is not a valid manifest").with_cause(e))?;

    let id = PluginId::new(raw.plugin.id).map_err(|e| bad(origin, e.message()))?;
    if raw.plugin.capabilities.is_empty() {
        return Err(bad(
            origin,
            format!("`{id}` declares no capabilities; a plugin that fills no slot is a no-op"),
        ));
    }

    let mut permissions = Vec::with_capacity(raw.permissions.len());
    for entry in &raw.permissions {
        permissions.push(permission_from_raw(entry, origin)?);
    }

    let mut manifest = PluginManifest::new(id, raw.plugin.name, raw.plugin.version);
    manifest.description = raw.plugin.description;
    manifest.abi_version = parse_abi_version(&raw.plugin.abi_version, origin)?;
    manifest.capabilities = raw.plugin.capabilities;
    manifest.permissions = permissions;
    Ok(manifest)
}

// --- the wire types ------------------------------------------------------------------------

/// The file's shape, which is *not* [`PluginManifest`]'s shape: the manifest is flat while
/// the file has `[plugin]` plus `[[permissions]]`, and `Permission`'s adjacently-tagged
/// JSON form cannot express `network_http` with an absent scope at all.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    plugin: RawPlugin,
    #[serde(default)]
    permissions: Vec<RawPermission>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlugin {
    id: String,
    name: String,
    version: String,
    abi_version: String,
    #[serde(default)]
    description: String,
    capabilities: Vec<CapabilityKind>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPermission {
    permission: String,
    scope: Option<toml::Value>,
}

// --- conversion ----------------------------------------------------------------------------

fn permission_from_raw(
    raw: &RawPermission,
    origin: Option<&Origin>,
) -> rivet_core::Result<Permission> {
    let name = raw.permission.as_str();
    let scope = raw.scope.as_ref();
    match name {
        "fs_read" => Ok(Permission::FsRead(fs_scope(name, scope, origin)?)),
        "fs_write" => Ok(Permission::FsWrite(fs_scope(name, scope, origin)?)),
        // An absent scope is the widest grant, so it cannot be spelled as an empty list.
        "network_http" => match scope {
            None => Ok(Permission::NetworkHttp(None)),
            Some(value) => Ok(Permission::NetworkHttp(Some(
                StringSet::new(host_list(name, value, origin)?)
                    .map_err(|e| bad(origin, format!("permission `{name}`: {}", e.message())))?,
            ))),
        },
        "secrets_read" => match scope {
            None => Err(bad(
                origin,
                "permission `secrets_read` needs a `scope` naming the keys it reads",
            )),
            Some(value) => Ok(Permission::SecretsRead(
                StringSet::new(host_list(name, value, origin)?)
                    .map_err(|e| bad(origin, format!("permission `{name}`: {}", e.message())))?,
            )),
        },
        "process_spawn" => scopeless(Permission::ProcessSpawn, name, scope, origin),
        "session_read" => scopeless(Permission::SessionRead, name, scope, origin),
        "session_write" => scopeless(Permission::SessionWrite, name, scope, origin),
        // Like `network_http`: an absent scope is the widest grant, so it cannot be
        // spelled as an empty list. Unlike it, the entries are topic prefixes.
        "events_subscribe" => match scope {
            None => Ok(Permission::EventsSubscribe(None)),
            Some(value) => Ok(Permission::EventsSubscribe(Some(
                TopicScope::new(topic_list(name, value, origin)?)
                    .map_err(|e| bad(origin, format!("permission `{name}`: {}", e.message())))?,
            ))),
        },
        "events_publish" => scopeless(Permission::EventsPublish, name, scope, origin),
        "job_manage" => scopeless(Permission::JobManage, name, scope, origin),
        other => Err(bad(
            origin,
            format!("unknown permission `{other}`; expected one of {PERMISSION_NAMES}"),
        )),
    }
}

/// A topic-prefix allowlist.
///
/// Rejects an empty prefix as well as an empty list: `""` matches every topic, so a grant
/// spelling it would read as narrow and behave as `None`. That is the same trap
/// `FsScope::subtree` rejects for `..`.
fn topic_list(
    name: &str,
    value: &toml::Value,
    origin: Option<&Origin>,
) -> rivet_core::Result<Vec<String>> {
    // `host_list` rejects an empty list too, but for the opposite reason: for hosts an
    // empty allowlist grants nothing, while an empty *topic* filter is read by
    // `topic_matches` as every topic. Same refusal, and the message it carries is about
    // hosts, so say the topic reason here.
    let value_is_empty = matches!(value, toml::Value::Array(items) if items.is_empty());
    if value_is_empty {
        return Err(bad(
            origin,
            format!(
                "permission `{name}` has an empty `scope`; an empty topic filter matches \
                 every topic, so leave `scope` out to ask for all of them"
            ),
        ));
    }
    let topics = host_list(name, value, origin)?;
    if topics.iter().any(String::is_empty) {
        return Err(bad(
            origin,
            format!(
                "permission `{name}` has an empty topic prefix in its `scope`; \
                 an empty prefix matches every topic, so it grants what leaving \
                 `scope` out grants"
            ),
        ));
    }
    Ok(topics)
}

fn fs_scope(
    name: &str,
    scope: Option<&toml::Value>,
    origin: Option<&Origin>,
) -> rivet_core::Result<FsScope> {
    let expected = format!(
        "permission `{name}` needs a `scope`: \"workspace\", \"anywhere\", or {{ subtree = \"...\" }}"
    );
    let Some(value) = scope else {
        return Err(bad(origin, expected));
    };
    match value {
        toml::Value::String(text) if text == "workspace" => Ok(FsScope::Workspace),
        toml::Value::String(text) if text == "anywhere" => Ok(FsScope::Anywhere),
        toml::Value::Table(table) => match (table.len(), table.get("subtree")) {
            (1, Some(toml::Value::String(path))) => {
                // Through the constructor, not the variant: `{ subtree = "../../../etc" }`
                // has to fail here and not survive as a grant nobody meant to give.
                FsScope::subtree(path.clone()).map_err(|e| bad(origin, e.message()))
            }
            _ => Err(bad(origin, expected)),
        },
        _ => Err(bad(origin, expected)),
    }
}

fn host_list(
    name: &str,
    value: &toml::Value,
    origin: Option<&Origin>,
) -> rivet_core::Result<Vec<String>> {
    let toml::Value::Array(items) = value else {
        return Err(bad(
            origin,
            format!("permission `{name}` expects `scope` to be an array of strings"),
        ));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(bad(
                origin,
                format!("permission `{name}` expects `scope` to be an array of strings"),
            ));
        };
        out.push(text.to_string());
    }
    if out.is_empty() {
        return Err(bad(
            origin,
            format!(
                "permission `{name}` has an empty `scope`; an empty list grants nothing, \
                 which is never what a manifest means"
            ),
        ));
    }
    Ok(out)
}

fn scopeless(
    permission: Permission,
    name: &str,
    scope: Option<&toml::Value>,
    origin: Option<&Origin>,
) -> rivet_core::Result<Permission> {
    if scope.is_some() {
        return Err(bad(origin, format!("permission `{name}` takes no `scope`")));
    }
    Ok(permission)
}

/// `major.minor`, matching [`CapabilityVersion`]'s two fields exactly.
///
/// Patch is not accepted rather than ignored: `abi_version = "0.1.2"` would otherwise
/// look like it said something the host silently dropped.
fn parse_abi_version(text: &str, origin: Option<&Origin>) -> rivet_core::Result<CapabilityVersion> {
    let malformed = || {
        bad(
            origin,
            format!("`abi_version = \"{text}\"` must be `major.minor`, e.g. \"0.1\""),
        )
    };
    let mut parts = text.split('.');
    let (Some(major), Some(minor), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(malformed());
    };
    let (Ok(major), Ok(minor)) = (major.parse::<u16>(), minor.parse::<u16>()) else {
        return Err(malformed());
    };
    Ok(CapabilityVersion::new(major, minor))
}

/// A manifest error, prefixed with where the manifest came from.
fn bad(origin: Option<&Origin>, message: impl fmt::Display) -> Error {
    match origin {
        Some(origin) => Error::plugin(format!("{origin}: {message}")),
        None => Error::plugin(format!("rivet-plugin.toml: {message}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest with `body` appended to a valid `[plugin]` table.
    fn with(body: &str) -> rivet_core::Result<PluginManifest> {
        parse(&format!(
            "[plugin]\n\
             id = \"acme.tool-lint\"\n\
             name = \"Lint\"\n\
             version = \"0.1.0\"\n\
             abi_version = \"0.1\"\n\
             capabilities = [\"tool\"]\n\
             {body}"
        ))
    }

    #[test]
    fn a_full_manifest_round_trips() {
        let manifest = parse(
            r#"
[plugin]
id           = "rivet.tool-git"
name         = "Git"
version      = "0.2.0"
abi_version  = "0.1"
description  = "Git status, diff, log and commit tools."
capabilities = ["tool", "policy"]

[[permissions]]
permission = "fs_read"
scope      = "workspace"

[[permissions]]
permission = "process_spawn"
"#,
        )
        .expect("the documented shape parses");

        assert_eq!(manifest.id.as_str(), "rivet.tool-git");
        assert_eq!(manifest.name, "Git");
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(
            manifest.description,
            "Git status, diff, log and commit tools."
        );
        assert_eq!(manifest.abi_version, CapabilityVersion::new(0, 1));
        assert_eq!(
            manifest.capabilities,
            [CapabilityKind::Tool, CapabilityKind::Policy]
        );
        assert_eq!(
            manifest.permissions,
            [
                Permission::FsRead(FsScope::Workspace),
                Permission::ProcessSpawn
            ]
        );
    }

    #[test]
    fn every_permission_variant_has_a_spelling() {
        let manifest = with(
            r#"
[[permissions]]
permission = "fs_read"
scope      = "anywhere"

[[permissions]]
permission = "fs_write"
scope      = { subtree = "docs/api" }

[[permissions]]
permission = "network_http"

[[permissions]]
permission = "secrets_read"
scope      = ["ACME_TOKEN"]

[[permissions]]
permission = "process_spawn"

[[permissions]]
permission = "session_read"

[[permissions]]
permission = "session_write"

[[permissions]]
permission = "events_subscribe"

[[permissions]]
permission = "events_publish"

[[permissions]]
permission = "job_manage"
"#,
        )
        .expect("every vocabulary entry parses");

        assert_eq!(
            manifest.permissions,
            [
                Permission::FsRead(FsScope::Anywhere),
                Permission::FsWrite(FsScope::Subtree("docs/api".into())),
                Permission::NetworkHttp(None),
                Permission::SecretsRead(StringSet::new(["ACME_TOKEN".to_string()]).unwrap()),
                Permission::ProcessSpawn,
                Permission::SessionRead,
                Permission::SessionWrite,
                Permission::EventsSubscribe(None),
                Permission::EventsPublish,
                Permission::JobManage,
            ]
        );
    }

    #[test]
    fn events_subscribe_takes_a_topic_allowlist() {
        let manifest = with(
            "[[permissions]]\n\
             permission = \"events_subscribe\"\n\
             scope = [\"tool.\", \"agent.run.\"]\n",
        )
        .unwrap();
        assert_eq!(
            manifest.permissions,
            [Permission::EventsSubscribe(Some(
                TopicScope::new(["tool.".to_string(), "agent.run.".to_string()]).unwrap()
            ))]
        );
    }

    #[test]
    fn an_empty_topic_scope_is_refused_for_the_topic_reason() {
        // `host_list` also rejects an empty list, but its message says an empty allowlist
        // "grants nothing" -- for topics an empty filter grants *everything*, the exact
        // inversion. Without this the block saying so can be deleted and every test still
        // passes.
        let err = with(
            "[[permissions]]\n\
             permission = \"events_subscribe\"\n\
             scope = []\n",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("matches every topic"),
            "the message must give the topic reason, not the host one: {err}"
        );
    }

    #[test]
    fn an_empty_topic_prefix_is_refused() {
        // `""` matches every topic, so a manifest spelling it would look narrow and behave
        // like leaving `scope` out entirely.
        let err = with(
            "[[permissions]]\n\
             permission = \"events_subscribe\"\n\
             scope = [\"tool.\", \"\"]\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty topic prefix"), "{err}");
    }

    #[test]
    fn network_http_takes_an_allowlist() {
        let manifest = with(
            "[[permissions]]\n\
             permission = \"network_http\"\n\
             scope = [\"api.openai.com\", \"api.deepseek.com\"]\n",
        )
        .unwrap();
        assert_eq!(
            manifest.permissions,
            [Permission::NetworkHttp(Some(
                StringSet::new(["api.openai.com".to_string(), "api.deepseek.com".to_string()])
                    .unwrap()
            ))]
        );
    }

    #[test]
    fn an_escaping_subtree_is_rejected_at_parse() {
        // Not left for `FsScope::meet` to drop: the plugin would then load with a
        // silently empty grant instead of its author being told the manifest is wrong.
        let err = with(
            "[[permissions]]\n\
             permission = \"fs_write\"\n\
             scope = { subtree = \"../../../etc\" }\n",
        )
        .unwrap_err();
        assert!(err.message().contains("../../../etc"), "{err}");
    }

    #[test]
    fn an_empty_allowlist_is_rejected() {
        for body in [
            "[[permissions]]\npermission = \"network_http\"\nscope = []\n",
            "[[permissions]]\npermission = \"secrets_read\"\nscope = []\n",
        ] {
            let err = with(body).unwrap_err();
            assert!(err.message().contains("empty `scope`"), "{err}");
        }
    }

    #[test]
    fn a_scope_on_a_scopeless_permission_is_rejected() {
        let err = with(
            "[[permissions]]\n\
             permission = \"process_spawn\"\n\
             scope = \"workspace\"\n",
        )
        .unwrap_err();
        assert!(err.message().contains("takes no `scope`"), "{err}");
    }

    #[test]
    fn a_missing_fs_scope_is_rejected() {
        let err = with("[[permissions]]\npermission = \"fs_read\"\n").unwrap_err();
        assert!(err.message().contains("needs a `scope`"), "{err}");
    }

    #[test]
    fn a_misspelled_permission_names_the_vocabulary() {
        let err = with("[[permissions]]\npermission = \"fs_reed\"\n").unwrap_err();
        assert!(err.message().contains("fs_reed"), "{err}");
        assert!(err.message().contains("fs_read"), "{err}");
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        // A typo that becomes a silently absent setting is the failure worth being strict
        // about, in a manifest as much as in `rivet.toml`.
        let err = parse(
            "[plugin]\n\
             id = \"acme.tool-lint\"\n\
             name = \"Lint\"\n\
             version = \"0.1.0\"\n\
             abi_version = \"0.1\"\n\
             capabilitys = [\"tool\"]\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("capabilitys"), "{err}");
    }

    #[test]
    fn an_unknown_top_level_table_is_rejected() {
        let err = with("[sandbox]\nprovider = \"local\"\n").unwrap_err();
        assert!(err.to_string().contains("sandbox"), "{err}");
    }

    #[test]
    fn an_unknown_capability_kind_names_the_token() {
        let err = parse(
            "[plugin]\n\
             id = \"acme.tool-lint\"\n\
             name = \"Lint\"\n\
             version = \"0.1.0\"\n\
             abi_version = \"0.1\"\n\
             capabilities = [\"tolls\"]\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("tolls"), "{err}");
    }

    #[test]
    fn no_capabilities_is_rejected() {
        let err = parse(
            "[plugin]\n\
             id = \"acme.tool-lint\"\n\
             name = \"Lint\"\n\
             version = \"0.1.0\"\n\
             abi_version = \"0.1\"\n\
             capabilities = []\n",
        )
        .unwrap_err();
        assert!(err.message().contains("no capabilities"), "{err}");
    }

    #[test]
    fn a_malformed_abi_version_names_the_token() {
        for text in ["0", "x.y", "0.1.2", "", "0."] {
            let err = parse(&format!(
                "[plugin]\n\
                 id = \"acme.tool-lint\"\n\
                 name = \"Lint\"\n\
                 version = \"0.1.0\"\n\
                 abi_version = \"{text}\"\n\
                 capabilities = [\"tool\"]\n"
            ))
            .unwrap_err();
            assert!(err.message().contains("major.minor"), "{text}: {err}");
        }
    }

    #[test]
    fn a_malformed_id_is_rejected() {
        let err = parse(
            "[plugin]\n\
             id = \"toollint\"\n\
             name = \"Lint\"\n\
             version = \"0.1.0\"\n\
             abi_version = \"0.1\"\n\
             capabilities = [\"tool\"]\n",
        )
        .unwrap_err();
        assert!(err.message().contains("namespace.name"), "{err}");
    }

    #[test]
    fn a_missing_field_is_rejected() {
        let err = parse("[plugin]\nid = \"acme.tool-lint\"\n").unwrap_err();
        assert!(err.to_string().contains("name"), "{err}");
    }

    #[test]
    fn the_origin_is_named_in_every_error() {
        let origin = Origin::Builtin {
            crate_name: "acme-tool-lint",
        };
        let err = parse_from("not toml at all", &origin).unwrap_err();
        assert!(err.message().contains("builtin(acme-tool-lint)"), "{err}");
    }
}
