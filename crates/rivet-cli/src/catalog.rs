//! The plugin catalog this binary ships, and the host side of loading it.
//!
//! In-process plugins are linked, so the set of *loadable* plugins is fixed at compile
//! time and discovery enumerates this list rather than scanning a directory. Adding a
//! plugin is one [`PluginSource`] here plus a workspace member — which is exactly the one
//! line `rivet plugin new` prints.
//!
//! This lives in the CLI rather than in `rivet-runtime` because the dependency arrow runs
//! `tool-filesystem -> rivet-runtime`. Putting the catalog in the runtime would point it
//! back at the plugins and close the cycle.

use std::sync::Arc;

use rivet_context_builtin::ContextPlugin;
use rivet_core::error::Error;
use rivet_core::id::PluginId;
use rivet_model_openai::OpenAiPlugin;
use rivet_plugin::{PluginLoader, PluginSource};
use rivet_runtime::{BroadcastBus, Registry};
use rivet_telemetry_log::TelemetryLogPlugin;
use rivet_tool_filesystem::FilesystemPlugin;

use crate::config::{Config, PluginSelection, Profile};

/// The plugin id of the built-in context providers.
///
/// Not switchable: the loop cannot assemble a system prompt without it, so it is added to
/// the selection whether or not `[plugins].enabled` mentions it.
pub const CONTEXT_PLUGIN_ID: &str = "rivet.context-builtin";

/// Every plugin linked into this binary.
#[must_use]
pub fn sources() -> Vec<PluginSource> {
    vec![
        PluginSource::builtin(
            "rivet-model-openai",
            rivet_model_openai::MANIFEST_TOML,
            |manifest| Arc::new(OpenAiPlugin::new(manifest)),
        ),
        PluginSource::builtin(
            "rivet-tool-filesystem",
            rivet_tool_filesystem::MANIFEST_TOML,
            |manifest| Arc::new(FilesystemPlugin::new(manifest)),
        ),
        PluginSource::builtin(
            "rivet-context-builtin",
            rivet_context_builtin::MANIFEST_TOML,
            |manifest| Arc::new(ContextPlugin::new(manifest)),
        ),
        PluginSource::builtin(
            "rivet-telemetry-log",
            rivet_telemetry_log::MANIFEST_TOML,
            |manifest| Arc::new(TelemetryLogPlugin::new(manifest)),
        ),
    ]
}

/// What an absent or empty `[plugins].enabled` selects.
///
/// **Not "the whole catalog".** `config.rs` wrote down why `PluginSelection::All` exists:
/// so that `rivet "explain this repo"` works in a directory with no `rivet.toml`. What
/// that needs is a model, tools and context. An observation sidecar is a different thing,
/// and switching it on for everyone who never wrote a config is not what "all" was for.
///
/// A plugin outside this list is still **discovered**, still appears in `rivet plugin
/// list`, and still loads the moment `enabled` names it. Phase 2's rule — an id either
/// loads or is a typo — is untouched; what this list decides is the *default selection*,
/// not what the build provides.
///
/// The distinction is not for one plugin: `docs/plan.md`'s MVP boundary table puts
/// `telemetry(otel · prometheus)` in MVP+, so at least two more of the same shape are
/// coming.
#[must_use]
pub fn default_selection() -> Vec<PluginId> {
    ["rivet.model-openai", "rivet.tool-filesystem"]
        .iter()
        .map(|id| PluginId::new(*id).expect("a literal default-selection id is valid"))
        .chain(std::iter::once(context_plugin_id()))
        .collect()
}

/// A registry with the configured plugins loaded, and the loader that owns them.
#[derive(Debug)]
pub struct Host {
    pub registry: Registry,
    pub bus: BroadcastBus,
    pub loader: PluginLoader,
}

impl Host {
    /// Signal every plugin's background work to stop, without unloading.
    pub fn shutdown(&self) {
        self.loader.shutdown();
    }
}

/// Discover and validate the catalog without constructing anything.
///
/// This is what `rivet plugin list` and `rivet plugin show` run on: no plugin is
/// instantiated, so neither command needs an API key or has any side effect.
///
/// # Errors
/// A manifest in this build that does not parse, or two catalog entries claiming one id.
pub fn inspect(profile: Profile) -> rivet_core::Result<PluginLoader> {
    let bus = BroadcastBus::new();
    let registry = Registry::new(bus);
    let events = registry.events();
    let mut loader = PluginLoader::new(
        registry,
        events,
        rivet_core::ABI_VERSION,
        profile.permissions(),
    );
    loader.discover(&sources())?;
    loader.validate();
    Ok(loader)
}

/// Load the configured plugins into a fresh registry on `bus`.
///
/// The bus is a parameter, not something this function makes. Who creates it decides when
/// an observer can be attached to it, and everything Phase 3 wants out of `--jsonl` — the
/// `runtime.started` line, the whole plugin lifecycle — happens *inside this call*. A bus
/// made here would be one nobody could be listening to yet.
///
/// # Errors
/// An enabled id this build does not provide, or any plugin failing to load. Every plugin
/// is attempted and every failure is named, because an operator with a five-plugin config
/// should not have to fix them one run at a time. Whatever did load is unloaded before the
/// error is returned, so a half-loaded process does not linger.
///
/// That includes a tool whose schema uses a keyword the runtime does not enforce:
/// [`rivet_runtime::schema::validate_spec`] runs inside `register_tool`, so such a tool
/// fails its own registration and the plugin carrying it is rolled back like any other.
pub async fn load(config: &Config, bus: BroadcastBus) -> rivet_core::Result<Host> {
    let registry = Registry::new(bus.clone());
    let events = registry.events();
    let mut loader = PluginLoader::new(
        registry.clone(),
        events,
        rivet_core::ABI_VERSION,
        config.profile.permissions(),
    );
    loader.discover(&sources())?;
    loader.validate();

    let selected = select(&loader, &config.plugins)?;
    let report = loader
        .load_selected(&selected, &|id| config.plugin_config(id))
        .await;

    if !report.failed.is_empty() {
        let detail = report
            .failed
            .iter()
            .map(|(id, error)| format!("`{id}`: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        loader.unload_all().await;
        loader.shutdown();
        return Err(Error::plugin(format!(
            "{} of {} plugin(s) failed to load — {detail}",
            report.failed.len(),
            selected.len()
        )));
    }

    Ok(Host {
        registry,
        bus,
        loader,
    })
}

/// Resolve `[plugins].enabled` against what this build actually provides.
///
/// # Errors
/// An id no catalog entry provides. After Phase 2 there is no middle category: an id
/// either loads or is a typo, so this lists the ids that exist rather than warning and
/// skipping.
pub fn select(
    loader: &PluginLoader,
    selection: &PluginSelection,
) -> rivet_core::Result<Vec<PluginId>> {
    let known: Vec<PluginId> = loader
        .records()
        .iter()
        .map(|record| record.id.clone())
        .collect();

    let mut chosen = match selection {
        PluginSelection::All => default_selection(),
        PluginSelection::Only(ids) => {
            for id in ids {
                if !known.contains(id) {
                    return Err(Error::invalid_argument(format!(
                        "`[plugins].enabled` names `{id}`, which this build does not \
                         provide. Available: {}.",
                        loader.known_ids()
                    )));
                }
            }
            ids.clone()
        }
    };

    let context = context_plugin_id();
    if !chosen.contains(&context) {
        chosen.push(context);
    }
    Ok(chosen)
}

/// # Panics
/// Never: [`CONTEXT_PLUGIN_ID`] is a valid plugin id and a test says so.
#[must_use]
pub fn context_plugin_id() -> PluginId {
    PluginId::new(CONTEXT_PLUGIN_ID).expect("CONTEXT_PLUGIN_ID is a valid plugin id")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Overrides;

    #[test]
    fn every_catalog_manifest_parses_and_declares_a_unique_id() {
        // The catalog and the manifests are two halves of one fact; a mismatch means an
        // id that passes validation and then fails at startup.
        let loader = inspect(Profile::Developer).expect("the shipped catalog is well formed");
        let mut ids: Vec<&str> = loader
            .records()
            .iter()
            .map(|record| record.id.as_str())
            .collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate catalog ids");
        assert_eq!(ids.len(), sources().len());
    }

    #[test]
    fn the_context_plugin_is_in_the_catalog_and_always_selected() {
        let loader = inspect(Profile::Developer).unwrap();
        assert!(loader.record(&context_plugin_id()).is_some());

        let only_the_model =
            PluginSelection::Only(vec![PluginId::new("rivet.model-openai").unwrap()]);
        let chosen = select(&loader, &only_the_model).unwrap();
        assert!(
            chosen.contains(&context_plugin_id()),
            "without it there is no system prompt: {chosen:?}"
        );
    }

    #[test]
    fn an_absent_enabled_list_selects_the_default_not_the_whole_catalog() {
        // Renamed from `..._selects_the_whole_catalog`, which stopped being true when
        // `default_selection` narrowed what `All` means. The catalog is what this build
        // *can* load; the default selection is what it loads when nobody said.
        let loader = inspect(Profile::Developer).unwrap();
        let chosen = select(&loader, &PluginSelection::All).unwrap();
        assert_eq!(chosen, default_selection());
        assert!(
            chosen.len() < sources().len(),
            "the catalog now carries a plugin the default does not enable"
        );
    }

    #[test]
    fn every_default_selection_id_is_in_the_catalog() {
        // `select`'s `All` branch does no validation -- only the `Only` branch checks ids
        // against the catalog. So a typo here would sail through `select` and fail in
        // `load_selected`, on *every* run, including the ones with no config file at all.
        let loader = inspect(Profile::Developer).unwrap();
        for id in default_selection() {
            assert!(
                loader.record(&id).is_some(),
                "`{id}` is in the default selection and not in the catalog: {}",
                loader.known_ids()
            );
        }
    }

    #[test]
    fn an_opt_in_plugin_is_absent_by_default_and_loads_when_named() {
        // The two halves of §7-8: the observation sidecar is in the catalog, so an
        // operator can enable it, and it is out of the default selection, so a tree with
        // no `rivet.toml` does not quietly start one.
        let telemetry = PluginId::new(rivet_telemetry_log::PLUGIN_ID).unwrap();
        let loader = inspect(Profile::Developer).unwrap();
        assert!(
            loader.record(&telemetry).is_some(),
            "it has to be discoverable, or `enabled` naming it would be a typo"
        );
        assert!(
            !select(&loader, &PluginSelection::All)
                .unwrap()
                .contains(&telemetry)
        );

        let named = PluginSelection::Only(vec![telemetry.clone()]);
        assert!(select(&loader, &named).unwrap().contains(&telemetry));
    }

    #[test]
    fn an_id_this_build_does_not_provide_names_the_ones_it_does() {
        let loader = inspect(Profile::Developer).unwrap();
        let typo = PluginSelection::Only(vec![PluginId::new("rivet.tool-filesystm").unwrap()]);
        let err = select(&loader, &typo).unwrap_err();
        assert!(err.message().contains("rivet.tool-filesystm"), "{err}");
        assert!(err.message().contains("rivet.tool-filesystem"), "{err}");
        assert_eq!(
            err.kind(),
            rivet_core::error::ErrorKind::InvalidArgument,
            "a typo in the config is exit code 2, like an unknown profile"
        );
    }

    #[test]
    fn every_id_the_shipped_example_enables_resolves() {
        // The example is the most likely first config a user has, and after Phase 2 an id
        // it lists is a startup error rather than a warning. This is the test that keeps
        // the example honest.
        let dir = tempfile::tempdir().unwrap();
        let example = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rivet.example.toml"),
        )
        .expect("rivet.example.toml");
        std::fs::write(dir.path().join(crate::config::FILE_NAME), &example).unwrap();
        let config = Config::load(dir.path(), &Overrides::default()).unwrap();

        let loader = inspect(config.profile).unwrap();
        select(&loader, &config.plugins).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn a_readonly_profile_strips_the_filesystem_plugins_write_permission() {
        // DoD 3 at the loader end, before anything is constructed.
        use rivet_core::capability::{FsScope, Permission};

        let id = PluginId::new("rivet.tool-filesystem").unwrap();
        let developer = inspect(Profile::Developer).unwrap();
        assert!(
            developer
                .record(&id)
                .unwrap()
                .effective
                .allows(&Permission::FsWrite(FsScope::Workspace))
        );

        let readonly = inspect(Profile::ReadOnly).unwrap();
        let record = readonly.record(&id).unwrap();
        assert!(
            !record
                .effective
                .allows(&Permission::FsWrite(FsScope::Workspace)),
            "the profile has to remove it, not the host's knowledge of this plugin"
        );
        assert!(
            record
                .denied
                .contains(&Permission::FsWrite(FsScope::Workspace)),
            "and `plugin show` has to be able to say so: {:?}",
            record.denied
        );
        assert!(
            record
                .effective
                .allows(&Permission::FsRead(FsScope::Workspace)),
            "reading survives"
        );
    }
}
