//! `rivet plugin list | show | new`.
//!
//! `list` and `show` stop at `VALIDATED`: they parse manifests, check the ABI and compute
//! `manifest ∩ profile`, but construct nothing. So neither needs an API key and neither
//! has a side effect — which is a change from Phase 1, where `rivet plugin list` went
//! through the whole load path and failed on a machine with no provider key.
//!
//! The price is that neither command can print what a plugin *registers*, only what it
//! declares. `rivet doctor` loads and prints the registrations.

use std::path::{Path, PathBuf};

use rivet_core::capability::{FsScope, Permission};
use rivet_core::error::Error;
use rivet_core::id::PluginId;
use rivet_plugin::PluginRecord;
use rivet_plugin::loader::state_label;

use crate::catalog;
use crate::config::Config;

const CARGO_TEMPLATE: &str = include_str!("../templates/plugin/Cargo.toml.tmpl");
const MANIFEST_TEMPLATE: &str = include_str!("../templates/plugin/rivet-plugin.toml.tmpl");
const LIB_TEMPLATE: &str = include_str!("../templates/plugin/lib.rs.tmpl");

/// One line per plugin this build provides.
///
/// # Errors
/// A manifest in this build that does not parse.
pub fn list(config: &Config) -> rivet_core::Result<()> {
    let loader = catalog::inspect(config.profile)?;
    // An id the config does not enable still gets a row -- the catalog is what this build
    // can load, not what this config asked for -- but the column says which is which.
    let enabled = catalog::select(&loader, &config.plugins)?;

    println!(
        "{:<10} {:<8} {:<24} {:<8} {:<5} {:<18} ORIGIN",
        "STATE", "ENABLED", "ID", "VERSION", "ABI", "CAPABILITIES"
    );
    for record in loader.records() {
        // `PluginId` and `CapabilityVersion` render through `write_str`/`write!`, which
        // ignore a width, so the columns are laid out over owned strings.
        println!(
            "{:<10} {:<8} {:<24} {:<8} {:<5} {:<18} {}",
            state_label(record.state),
            if enabled.contains(&record.id) {
                "yes"
            } else {
                "no"
            },
            record.id.as_str(),
            record.manifest.version,
            record.manifest.abi_version.to_string(),
            capabilities(record),
            record.origin,
        );
        if let Some(error) = &record.error {
            println!("  ! {error}");
        }
    }
    println!("\nrun `rivet doctor` to load them and see what each one registers");
    Ok(())
}

/// A plugin's manifest, capabilities and effective permissions.
///
/// # Errors
/// An id this build does not provide.
pub fn show(config: &Config, id: &str) -> rivet_core::Result<()> {
    let plugin_id = PluginId::new(id)?;
    let loader = catalog::inspect(config.profile)?;
    let record = loader.record(&plugin_id).ok_or_else(|| {
        Error::not_found(format!(
            "no plugin `{id}` in this build. Available: {}.",
            loader.known_ids()
        ))
    })?;
    let enabled = catalog::select(&loader, &config.plugins)?;

    println!(
        "{}  \"{}\" {}",
        record.id, record.manifest.name, record.manifest.version
    );
    if !record.manifest.description.is_empty() {
        println!("  {}", record.manifest.description);
    }
    println!(
        "\n  abi          {} (host {}) {}",
        record.manifest.abi_version,
        rivet_core::ABI_VERSION,
        if record.manifest.is_compatible_with(rivet_core::ABI_VERSION) {
            "ok"
        } else {
            "INCOMPATIBLE — this plugin is rejected before it can register anything"
        }
    );
    println!("  origin       {}", record.origin);
    println!("  state        {}", state_label(record.state));
    println!(
        "  enabled      {}",
        if enabled.contains(&record.id) {
            "yes"
        } else {
            "no (not in `[plugins].enabled`)"
        }
    );
    println!("  capabilities {}", capabilities(record));

    if record.manifest.permissions.is_empty() {
        println!("  permissions  (none requested)");
    } else {
        println!("  permissions  {:<26} effective", "requested");
        for wanted in &record.manifest.permissions {
            println!(
                "               {:<26} {}",
                describe(wanted),
                effect(record, wanted, config.profile.name())
            );
        }
    }
    if let Some(error) = &record.error {
        println!("  error        {error}");
    }
    Ok(())
}

/// Scaffold a new plugin crate in the working directory.
///
/// # Errors
/// An id that is not `namespace.name`, a directory that already exists, or an I/O failure.
pub fn new(id: &str) -> rivet_core::Result<()> {
    // Validate before touching disk: half a scaffold is worse than none.
    let plugin_id = PluginId::new(id)?;
    let crate_name = plugin_id.as_str().replace('.', "-");
    let dir = PathBuf::from(&crate_name);
    if dir.exists() {
        return Err(Error::invalid_argument(format!(
            "`{}` already exists; `rivet plugin new` will not write into it",
            dir.display()
        )));
    }

    let bare = plugin_id
        .as_str()
        .split_once('.')
        .map_or(plugin_id.as_str(), |(_, rest)| rest);
    let struct_name = format!("{}Plugin", pascal_case(bare));
    let module = crate_name.replace('-', "_");
    let render = |template: &str| {
        template
            .replace("{{id}}", plugin_id.as_str())
            .replace("{{name}}", &title_case(bare))
            .replace("{{crate_name}}", &crate_name)
            .replace("{{struct_name}}", &struct_name)
    };

    std::fs::create_dir_all(dir.join("src")).map_err(|e| io(&dir, e))?;
    write(&dir.join("Cargo.toml"), &render(CARGO_TEMPLATE))?;
    write(&dir.join("rivet-plugin.toml"), &render(MANIFEST_TEMPLATE))?;
    write(&dir.join("src/lib.rs"), &render(LIB_TEMPLATE))?;

    println!("created {crate_name}/{{Cargo.toml,rivet-plugin.toml,src/lib.rs}}");
    println!("next: add \"{crate_name}\" to the workspace members, then add one line to");
    println!("      crates/rivet-cli/src/catalog.rs:");
    println!("        PluginSource::builtin(\"{crate_name}\", {module}::MANIFEST_TOML,");
    println!("                              |m| Arc::new({module}::{struct_name}::new(m))),");
    println!("      (in-process plugins are linked; out-of-process loading is Phase 6)");
    Ok(())
}

// --- rendering helpers -------------------------------------------------------------------

fn capabilities(record: &PluginRecord) -> String {
    record
        .manifest
        .capabilities
        .iter()
        .map(|kind| {
            serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{kind:?}"))
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// What the profile did to one requested permission.
fn effect(record: &PluginRecord, wanted: &Permission, profile: &str) -> String {
    // Read from the record rather than re-asking the manifest. `validate` decided the ABI
    // question against the `host_abi` its loader was constructed with; asking
    // `is_compatible_with(rivet_core::ABI_VERSION)` here answers a *different* question
    // that happens to agree today because both CLI entry points pass that constant.
    if !record.permissions_computed {
        return "not evaluated (this plugin never passed validation)".to_string();
    }
    // No normalisation here, and none needed: `StringSet` and `TopicScope` are canonical
    // by construction, so `denied` (the manifest's own spelling) and `effective` (what
    // `meet` produced) are directly comparable. Three commits fixed a missing
    // `canonicalised()` at three call sites, this being the last of them; the newtypes
    // removed the call sites instead.
    if record.denied.contains(wanted) {
        format!("removed by profile `{profile}`")
    } else if record.effective.contains(wanted) {
        "granted".to_string()
    } else {
        // Met, but not to what was asked for: the profile capped a wider request.
        format!("narrowed by profile `{profile}`")
    }
}

/// A permission with its scope, in the canonical form.
///
/// Not the manifest's own spelling: `scope = ["tool.", "tool.execute."]` prints
/// `events_subscribe(tool.)`, because that is the set it asked for and the set it got.
fn describe(permission: &Permission) -> String {
    match permission {
        Permission::FsRead(scope) => format!("fs_read({})", fs_scope(scope)),
        Permission::FsWrite(scope) => format!("fs_write({})", fs_scope(scope)),
        Permission::ProcessSpawn => "process_spawn".to_string(),
        Permission::NetworkHttp(None) => "network_http(any host)".to_string(),
        Permission::NetworkHttp(Some(hosts)) => {
            format!("network_http({})", hosts.as_slice().join(" "))
        }
        Permission::SessionRead => "session_read".to_string(),
        Permission::SessionWrite => "session_write".to_string(),
        Permission::EventsSubscribe(None) => "events_subscribe(all topics)".to_string(),
        Permission::EventsSubscribe(Some(topics)) => {
            format!("events_subscribe({})", topics.as_slice().join(" "))
        }
        Permission::EventsPublish => "events_publish".to_string(),
        Permission::SecretsRead(keys) => format!("secrets_read({})", keys.as_slice().join(" ")),
        Permission::JobManage => "job_manage".to_string(),
    }
}

fn fs_scope(scope: &FsScope) -> String {
    match scope {
        FsScope::Subtree(path) => format!("subtree {path}"),
        FsScope::Workspace => "workspace".to_string(),
        FsScope::Anywhere => "anywhere".to_string(),
    }
}

/// `tool-lint` -> `ToolLint`.
fn pascal_case(text: &str) -> String {
    text.split(['-', '_', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect()
}

/// `tool-lint` -> `Tool lint`.
fn title_case(text: &str) -> String {
    let spaced = text.replace(['-', '_'], " ");
    let mut chars = spaced.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

fn write(path: &Path, contents: &str) -> rivet_core::Result<()> {
    std::fs::write(path, contents).map_err(|e| io(path, e))
}

fn io(path: &Path, error: std::io::Error) -> Error {
    Error::internal(format!("could not write `{}`", path.display())).with_cause(error)
}

#[cfg(test)]
mod tests {
    use rivet_core::capability::{CapabilityVersion, PermissionSet, StringSet, TopicScope};
    use rivet_core::plugin::{PluginManifest, PluginState};
    use rivet_plugin::Origin;

    use super::*;

    #[test]
    fn names_are_derived_from_the_id() {
        assert_eq!(pascal_case("tool-lint"), "ToolLint");
        assert_eq!(pascal_case("hello"), "Hello");
        assert_eq!(title_case("tool-lint"), "Tool lint");
    }

    /// A record for a plugin asking for `wanted`, as `validate` leaves it: `VALIDATED`,
    /// with `effective` and `denied` computed.
    fn record_asking_for(wanted: Vec<Permission>) -> PluginRecord {
        let id = PluginId::new("test.asker").unwrap();
        let manifest =
            PluginManifest::new(id.clone(), "Asker", "0.1.0").with_permissions(wanted.clone());
        PluginRecord {
            id,
            manifest,
            origin: Origin::Builtin {
                crate_name: "test-asker",
            },
            state: PluginState::Validated,
            instance_id: None,
            effective: PermissionSet::new(wanted),
            permissions_computed: true,
            denied: Vec::new(),
            registered: Vec::new(),
            claimed: Vec::new(),
            error: None,
        }
    }

    #[test]
    fn an_abi_rejected_plugin_does_not_blame_the_profile() {
        // `validate` rejects on the ABI *before* it computes `effective`/`denied`, so both
        // are empty on this record and every permission would otherwise fall through to
        // "narrowed by profile `developer`" -- naming a profile that did nothing. The ABI
        // line `show` prints above this one already said why.
        let mut record = record_asking_for(vec![Permission::FsRead(FsScope::Workspace)]);
        record.manifest.abi_version = CapabilityVersion::new(0, 99);
        record.state = PluginState::Failed;
        record.effective = PermissionSet::empty();
        record.permissions_computed = false;

        let said = effect(
            &record,
            &Permission::FsRead(FsScope::Workspace),
            "developer",
        );
        assert!(!said.contains("developer"), "{said}");
        assert!(said.contains("not evaluated"), "{said}");
    }

    #[test]
    fn a_host_abi_the_cli_does_not_share_does_not_make_a_grant_disappear() {
        // The record is the source of truth, not the manifest. `validate` answers the ABI
        // question against the `host_abi` its loader was built with, and `PluginLoader::new`
        // takes that as a parameter so an embedder can pass something other than
        // `rivet_core::ABI_VERSION`. Re-deriving here would call this record unevaluated and
        // print "not evaluated" for a permission the profile really did grant -- the exact
        // bug `permissions_computed` was added to stop.
        let mut record = record_asking_for(vec![Permission::FsRead(FsScope::Workspace)]);
        record.manifest.abi_version = CapabilityVersion::new(0, 99);
        assert!(
            !record.manifest.is_compatible_with(rivet_core::ABI_VERSION),
            "the manifest and the CLI's constant must disagree for this test to mean anything"
        );

        let said = effect(
            &record,
            &Permission::FsRead(FsScope::Workspace),
            "developer",
        );
        assert_eq!(said, "granted");
    }

    #[test]
    fn a_profile_that_capped_a_request_is_named() {
        // The other side of that branch: here the profile really is the reason. Asked for
        // `anywhere`, met down to the workspace -- neither denied outright nor granted as
        // asked, which is the one `effect` arm no test used to reach.
        let mut record = record_asking_for(vec![Permission::FsRead(FsScope::Anywhere)]);
        record.effective = PermissionSet::new([Permission::FsRead(FsScope::Workspace)]);

        let said = effect(&record, &Permission::FsRead(FsScope::Anywhere), "developer");
        assert_eq!(said, "narrowed by profile `developer`");
    }

    #[test]
    fn a_scope_granted_in_full_is_not_reported_as_narrowed() {
        // `meet` returns scope lists sorted; a manifest writes them in whatever order the
        // author chose. Comparing the two directly made `plugin show` report a permission
        // the profile granted whole as "narrowed by profile" -- a lie in the alarming
        // direction, in the command an operator audits with.
        for wanted in [
            Permission::NetworkHttp(Some(
                StringSet::new(["b.example.com".to_string(), "a.example.com".to_string()]).unwrap(),
            )),
            Permission::EventsSubscribe(Some(
                TopicScope::new(["tool.".to_string(), "agent.run.".to_string()]).unwrap(),
            )),
            Permission::SecretsRead(
                StringSet::new(["B_TOKEN".to_string(), "A_TOKEN".to_string()]).unwrap(),
            ),
        ] {
            let mut record = record_asking_for(vec![wanted.clone()]);
            // What the loader stores: the meet against an unrestricted profile grant.
            // No `canonicalised()` to call any more -- `wanted` already is one.
            record.effective = PermissionSet::new([wanted.clone()]);

            assert_eq!(
                effect(&record, &wanted, "developer"),
                "granted",
                "{wanted:?} was granted in full"
            );
        }
    }

    #[test]
    fn a_scope_removed_in_full_is_not_reported_as_narrowed() {
        // The other half of the comparison. `record.denied` holds what the manifest asked
        // for, in the author's order -- that is what it documents. Canonicalising only
        // `effective` made a permission the profile removed *whole* read as "narrowed",
        // which is the reassuring direction: an operator auditing with `plugin show`
        // believes the plugin kept part of a grant it has none of.
        //
        // No profile grants `secrets_read` at all, so every such manifest is removed in
        // full and the spelling is the only variable.
        for wanted in [
            Permission::SecretsRead(
                StringSet::new(["B_TOKEN".to_string(), "A_TOKEN".to_string()]).unwrap(),
            ),
            Permission::SecretsRead(
                StringSet::new(["A_TOKEN".to_string(), "B_TOKEN".to_string()]).unwrap(),
            ),
            // Absorption widens the surface: a *sorted* list can canonicalise to
            // something else too.
            Permission::EventsSubscribe(Some(
                TopicScope::new(["agent.text.".to_string(), "agent.text.delta".to_string()])
                    .unwrap(),
            )),
        ] {
            let mut record = record_asking_for(vec![wanted.clone()]);
            record.effective = PermissionSet::new([]);
            // Verbatim, exactly as `PluginLoader::validate` builds it.
            record.denied = vec![wanted.clone()];

            assert_eq!(
                effect(&record, &wanted, "readonly"),
                "removed by profile `readonly`",
                "{wanted:?} was removed in full, whatever order it was written in"
            );
        }
    }

    #[test]
    fn every_permission_has_a_rendering() {
        // The `plugin show` table is the only place an operator sees the scope of a
        // grant; a variant falling through to `{:?}` would print Rust, not a manifest.
        assert_eq!(
            describe(&Permission::FsWrite(FsScope::Subtree("docs".into()))),
            "fs_write(subtree docs)"
        );
        assert_eq!(
            describe(&Permission::NetworkHttp(None)),
            "network_http(any host)"
        );
        assert_eq!(
            describe(&Permission::NetworkHttp(Some(
                StringSet::new(["a.test".to_string()]).unwrap()
            ))),
            "network_http(a.test)"
        );
        assert_eq!(
            describe(&Permission::SecretsRead(
                StringSet::new(["K".to_string()]).unwrap()
            )),
            "secrets_read(K)"
        );
        assert_eq!(describe(&Permission::JobManage), "job_manage");
    }
}
