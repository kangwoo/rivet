//! `rivet.toml`, profiles, and everything decided before a session exists.
//!
//! The schema is the one `rivet.example.toml` already ships; this implements it rather
//! than inventing a second format. Unknown *sections* are accepted so a config written for
//! a later phase still loads, but unknown values in the places that matter — the profile
//! name, a deny glob, a plugin id — fail at startup. A typo that silently becomes a
//! permissive setting is the failure mode worth being strict about.
//!
//! Precedence: CLI flag > environment variable > file > built-in default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rivet_core::agent::RunLimits;
use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::error::Error;
use rivet_core::model::ModelId;
use rivet_core::workspace::Workspace;
use serde::Deserialize;

/// The file name searched for up the directory tree.
pub const FILE_NAME: &str = "rivet.toml";

/// Environment variable naming a config file explicitly.
pub const CONFIG_ENV: &str = "RIVET_CONFIG";

/// Where sessions live, relative to the workspace root. Already in `.gitignore`.
pub const SESSIONS_DIR: &str = ".rivet/sessions";

/// Plugin ids Phase 1 can actually register.
///
/// `rivet.context-builtin` is not in `rivet.example.toml` because it is not optional: the
/// loop cannot assemble a system prompt without it. Listing it is allowed and does
/// nothing.
pub const IMPLEMENTED_PLUGINS: [&str; 3] = [
    "rivet.model-openai",
    "rivet.tool-filesystem",
    "rivet.context-builtin",
];

/// Plugin ids the shipped example enables that later phases provide.
///
/// A config copied from the example is the most likely first config a user has, so these
/// warn and are skipped rather than failing the run. Phase 2's loader removes this
/// middle category: everything then either loads or is a typo.
pub const PLANNED_PLUGINS: [&str; 4] = [
    "rivet.tool-shell",
    "rivet.tool-git",
    "rivet.policy-default",
    "rivet.sandbox-local",
];

// --- the file ----------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    #[serde(default)]
    pub agent: AgentSection,
    #[serde(default)]
    pub workspace: WorkspaceSection,
    #[serde(default)]
    pub plugins: PluginsSection,
    #[serde(default)]
    pub policy: PolicySection,
    /// Parsed and carried, unused until Phase 4.
    #[serde(default)]
    pub sandbox: toml::Table,
    /// Parsed and carried, unused until Phase 5.
    #[serde(default)]
    pub task: toml::Table,
    /// Named agents. Parsed so a config that declares a reviewer still loads, but Phase 1
    /// has no surface for selecting one, so nothing here takes effect — including the
    /// `context_providers` names, which do not all exist yet.
    #[serde(default)]
    pub agents: BTreeMap<String, toml::Table>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AgentSection {
    pub model: String,
    /// Extra instructions prepended to the system prompt. Not in the shipped example;
    /// absent means the runtime preamble alone.
    pub instructions: String,
    pub limits: LimitsSection,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            model: "deepseek/deepseek-chat".to_string(),
            instructions: String::new(),
            limits: LimitsSection::default(),
        }
    }
}

/// The `[agent.limits]` table. Field names mirror [`RunLimits`] exactly, prefix and all:
/// a config key that does not match the contract it configures is a trap.
#[derive(Debug, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_field_names)]
pub struct LimitsSection {
    pub max_turns: u32,
    pub max_duration_ms: u64,
    pub max_total_tokens: u64,
    pub max_context_tokens: u32,
    pub max_consecutive_tool_errors: u32,
}

impl Default for LimitsSection {
    fn default() -> Self {
        let limits = RunLimits::default();
        Self {
            max_turns: limits.max_turns,
            max_duration_ms: limits.max_duration_ms,
            max_total_tokens: limits.max_total_tokens,
            max_context_tokens: limits.max_context_tokens,
            max_consecutive_tool_errors: limits.max_consecutive_tool_errors,
        }
    }
}

impl LimitsSection {
    #[must_use]
    pub fn to_limits(&self) -> RunLimits {
        RunLimits {
            max_turns: self.max_turns,
            max_duration_ms: self.max_duration_ms,
            max_total_tokens: self.max_total_tokens,
            max_context_tokens: self.max_context_tokens,
            max_consecutive_tool_errors: self.max_consecutive_tool_errors,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WorkspaceSection {
    /// Paths inside the workspace that are never read or written.
    pub deny: Vec<String>,
    /// Not in the shipped example. Absent means "the directory holding this file".
    pub root: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PluginsSection {
    #[serde(default)]
    pub enabled: Vec<String>,
    /// Per-plugin tables, keyed by plugin id.
    #[serde(flatten)]
    pub settings: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct PolicySection {
    pub profile: String,
}

impl Default for PolicySection {
    fn default() -> Self {
        Self {
            profile: "developer".to_string(),
        }
    }
}

// --- profiles -----------------------------------------------------------------------------

/// What an operator's profile narrows.
///
/// In Phase 1 a profile narrows the **agent's tool scope** — pipeline step 2 — and nothing
/// else. It is not a policy: there is no policy chain until Phase 4, and `--profile
/// readonly` must not be described as if it enforced something it does not. What it does
/// do is real: a tool that is never registered is never offered to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Developer,
    ReadOnly,
    Reviewer,
    Ci,
    Production,
}

impl Profile {
    /// Parse a profile name.
    ///
    /// # Errors
    /// An unknown name fails at startup rather than quietly falling back to a permissive
    /// default, which is how a typo becomes a security incident.
    pub fn parse(name: &str) -> rivet_core::Result<Self> {
        match name {
            "developer" => Ok(Self::Developer),
            "readonly" => Ok(Self::ReadOnly),
            "reviewer" => Ok(Self::Reviewer),
            "ci" => Ok(Self::Ci),
            "production" => Ok(Self::Production),
            other => Err(Error::invalid_argument(format!(
                "unknown profile `{other}`; expected one of \
                 developer, readonly, reviewer, ci, production"
            ))),
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Developer => "developer",
            Self::ReadOnly => "readonly",
            Self::Reviewer => "reviewer",
            Self::Ci => "ci",
            Self::Production => "production",
        }
    }

    /// Whether the write tool is registered at all.
    #[must_use]
    pub fn writable(self) -> bool {
        matches!(self, Self::Developer | Self::Ci)
    }

    /// The tool names in scope, or `None` for "every registered tool".
    #[must_use]
    pub fn tool_scope(self) -> Option<Vec<String>> {
        match self {
            Self::Developer | Self::Ci | Self::ReadOnly | Self::Production => None,
            // A reviewer that can edit the code is not a reviewer.
            Self::Reviewer => Some(vec![
                "read_file".to_string(),
                "list_dir".to_string(),
                "search".to_string(),
            ]),
        }
    }

    /// The grant this profile computes.
    ///
    /// Phase 2 intersects it with each plugin's manifest; this value is the `profile`
    /// operand of that meet, so it is already in the right shape.
    #[must_use]
    pub fn permissions(self) -> PermissionSet {
        let mut granted = vec![
            Permission::FsRead(FsScope::Workspace),
            Permission::SessionRead,
            Permission::SessionWrite,
            Permission::EventsPublish,
        ];
        if self.writable() {
            granted.push(Permission::FsWrite(FsScope::Workspace));
        }
        PermissionSet::new(granted)
    }

    /// Whether this profile alone means nobody can answer an approval prompt.
    ///
    /// Only `ci`. `docs/security.md`'s profile table puts `production` in the column where
    /// *everything* needs approval, so marking it unattended would, once Phase 4 wires
    /// approvals up, silently auto-deny every approvable action rather than ask. The
    /// operator checklist asks for `--headless` plus `ci` in CI, and that is what this
    /// implements.
    #[must_use]
    pub fn implies_unattended(self) -> bool {
        self == Self::Ci
    }
}

// --- the resolved configuration --------------------------------------------------------------

/// Sections a config may declare that Phase 1 parses but does not act on.
///
/// Reported by `rivet doctor` rather than ignored in silence: an operator who configured a
/// sandbox should be told it is not enforcing anything yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inert {
    pub sandbox: bool,
    pub task: bool,
    pub named_agents: Vec<String>,
}

/// Everything decided before a session is created.
#[derive(Debug)]
pub struct Config {
    /// The file this came from, if there was one.
    pub source: Option<PathBuf>,
    pub workspace: Workspace,
    pub sessions_dir: PathBuf,
    pub model: ModelId,
    pub instructions: String,
    pub limits: RunLimits,
    pub profile: Profile,
    pub unattended: bool,
    /// Implemented plugin ids to load, in a stable order.
    pub plugins: Vec<String>,
    /// Enabled ids a later phase will provide, reported once at startup.
    pub deferred_plugins: Vec<String>,
    /// Per-plugin settings, as JSON, keyed by plugin id.
    pub plugin_settings: BTreeMap<String, serde_json::Value>,
    /// Configured sections that later phases will act on.
    pub inert: Inert,
}

/// How the command line overrides the file.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub config_path: Option<PathBuf>,
    pub profile: Option<String>,
    pub headless: bool,
}

impl Config {
    /// Find, read and resolve the configuration.
    ///
    /// # Errors
    /// [`rivet_core::error::ErrorKind::InvalidArgument`] for a malformed file, an unknown
    /// profile, a malformed deny glob, an unknown plugin id, or a model id that is not
    /// `provider/model`. All of these are startup failures on purpose.
    pub fn load(cwd: &Path, overrides: &Overrides) -> rivet_core::Result<Self> {
        let source = locate(cwd, overrides.config_path.as_deref())?;
        let file = match &source {
            Some(path) => read(path)?,
            None => FileConfig::default(),
        };

        // The workspace root is the directory that holds the config, because "where the
        // rules live" is the most predictable reading of "what they govern". Not the git
        // root: Rivet has to work in directories that are not repositories, and a
        // repository root can be far wider than the config's author intended.
        let root = match (&file.workspace.root, &source) {
            (Some(explicit), _) => PathBuf::from(explicit),
            (None, Some(path)) => path
                .parent()
                .map_or_else(|| cwd.to_path_buf(), Path::to_path_buf),
            (None, None) => cwd.to_path_buf(),
        };
        let workspace = rivet_runtime::workspace::open(&root, file.workspace.deny.clone())?;

        let profile_name = overrides
            .profile
            .clone()
            .unwrap_or_else(|| file.policy.profile.clone());
        let profile = Profile::parse(&profile_name)?;

        let (plugins, deferred_plugins) = classify_plugins(&file.plugins.enabled)?;

        let mut plugin_settings = BTreeMap::new();
        for (id, value) in file.plugins.settings {
            if id == "enabled" {
                continue;
            }
            let json = serde_json::to_value(&value).map_err(|e| {
                Error::invalid_argument(format!("`[plugins.\"{id}\"]` is not valid")).with_cause(e)
            })?;
            plugin_settings.insert(id, json);
        }

        let sessions_dir = workspace.root().join(SESSIONS_DIR);
        let inert = Inert {
            sandbox: !file.sandbox.is_empty(),
            task: !file.task.is_empty(),
            // `[agents.*]` is parsed so a config declaring a reviewer still loads, but
            // Phase 1 has no way to select one -- and its `context_providers` names do not
            // all exist yet, so acting on it would fail at startup.
            named_agents: file.agents.keys().cloned().collect(),
        };

        Ok(Self {
            source,
            model: ModelId::new(file.agent.model)?,
            instructions: file.agent.instructions,
            limits: file.agent.limits.to_limits(),
            profile,
            // `--headless` is not a no-op waiting for Phase 4: the value flows to exactly
            // where `PolicyRequest.unattended` will read it, and `rivet doctor` shows it.
            unattended: overrides.headless || profile.implies_unattended(),
            plugins,
            deferred_plugins,
            plugin_settings,
            workspace,
            sessions_dir,
            inert,
        })
    }

    /// The settings table for one plugin, as JSON.
    #[must_use]
    pub fn settings_for(&self, plugin_id: &str) -> serde_json::Value {
        self.plugin_settings
            .get(plugin_id)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    }

    /// The environment variable the model plugin expects to hold its key.
    #[must_use]
    pub fn api_key_env(&self) -> String {
        self.settings_for("rivet.model-openai")
            .get("api_key_env")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("OPENAI_API_KEY")
            .to_string()
    }

    /// Check that the model's credential is present.
    ///
    /// Called before a session is created, so a missing key does not leave an empty
    /// session behind and the operator is told which variable to set.
    ///
    /// # Errors
    /// [`rivet_core::error::ErrorKind::InvalidArgument`] naming the variable.
    pub fn check_credentials(&self) -> rivet_core::Result<()> {
        let name = self.api_key_env();
        if std::env::var(&name).is_ok_and(|v| !v.is_empty()) {
            return Ok(());
        }
        Err(Error::invalid_argument(format!(
            "the environment variable `{name}` is not set; it must hold the API key for \
             `{}`. Set it, or point `api_key_env` at a different variable in {}.",
            self.model,
            self.source
                .as_ref()
                .map_or_else(|| FILE_NAME.to_string(), |p| p.display().to_string())
        )))
    }
}

/// Find the config file: explicit flag, then the environment, then up the tree.
fn locate(cwd: &Path, explicit: Option<&Path>) -> rivet_core::Result<Option<PathBuf>> {
    if let Some(path) = explicit {
        if !path.exists() {
            return Err(Error::not_found(format!(
                "no configuration file at `{}`",
                path.display()
            )));
        }
        return Ok(Some(path.to_path_buf()));
    }
    if let Ok(from_env) = std::env::var(CONFIG_ENV) {
        let path = PathBuf::from(from_env);
        if !path.exists() {
            return Err(Error::not_found(format!(
                "{CONFIG_ENV} points at `{}`, which does not exist",
                path.display()
            )));
        }
        return Ok(Some(path));
    }
    let mut dir = Some(cwd);
    while let Some(current) = dir {
        let candidate = current.join(FILE_NAME);
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
        dir = current.parent();
    }
    Ok(None)
}

fn read(path: &Path) -> rivet_core::Result<FileConfig> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::not_found(format!("could not read `{}`", path.display())).with_cause(e)
    })?;
    toml::from_str(&text).map_err(|e| {
        Error::invalid_argument(format!("`{}` is not valid", path.display())).with_cause(e)
    })
}

/// Split enabled plugin ids into what loads now, what waits for a later phase, and typos.
fn classify_plugins(enabled: &[String]) -> rivet_core::Result<(Vec<String>, Vec<String>)> {
    let mut load = Vec::new();
    let mut deferred = Vec::new();

    if enabled.is_empty() {
        return Ok((
            IMPLEMENTED_PLUGINS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            deferred,
        ));
    }

    for id in enabled {
        if IMPLEMENTED_PLUGINS.contains(&id.as_str()) {
            if !load.contains(id) {
                load.push(id.clone());
            }
        } else if PLANNED_PLUGINS.contains(&id.as_str()) {
            deferred.push(id.clone());
        } else {
            return Err(Error::invalid_argument(format!(
                "unknown plugin id `{id}` in `[plugins].enabled`. \
                 Available now: {}. Planned: {}.",
                IMPLEMENTED_PLUGINS.join(", "),
                PLANNED_PLUGINS.join(", ")
            )));
        }
    }

    // Context providers are not optional: without them there is no system prompt.
    let builtin = "rivet.context-builtin".to_string();
    if !load.contains(&builtin) {
        load.push(builtin);
    }
    Ok((load, deferred))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn the_shipped_example_loads() {
        // The example file is the most likely first configuration a user has. If it stops
        // loading, the documented starting point is broken.
        let dir = tempfile::tempdir().unwrap();
        let example = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rivet.example.toml"),
        )
        .expect("rivet.example.toml");
        write_config(dir.path(), &example);

        let config = Config::load(dir.path(), &Overrides::default()).expect("the example loads");
        assert_eq!(config.model.as_str(), "deepseek/deepseek-chat");
        assert_eq!(config.profile, Profile::Developer);
        assert_eq!(config.limits.max_turns, 50);
        assert!(config.workspace.resolve(Path::new(".env")).is_err());
        assert_eq!(config.api_key_env(), "DEEPSEEK_API_KEY");
        assert_eq!(
            config.deferred_plugins,
            [
                "rivet.tool-shell",
                "rivet.tool-git",
                "rivet.policy-default",
                "rivet.sandbox-local"
            ],
            "four of the six enabled ids are later phases, and are skipped with a warning"
        );
        assert_eq!(
            config.plugins,
            [
                "rivet.model-openai",
                "rivet.tool-filesystem",
                "rivet.context-builtin"
            ]
        );
    }

    #[test]
    fn the_shipped_example_keeps_its_named_agent() {
        // `[agents.reviewer]` names context providers Phase 1 does not register. Parsing
        // and ignoring it keeps the example usable; acting on it would fail at startup.
        let dir = tempfile::tempdir().unwrap();
        let example = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rivet.example.toml"),
        )
        .unwrap();
        write_config(dir.path(), &example);
        let config = Config::load(dir.path(), &Overrides::default()).unwrap();
        assert_eq!(config.inert.named_agents, ["reviewer"]);
        assert!(
            config.inert.sandbox,
            "and the sandbox section is carried too"
        );
    }

    #[test]
    fn the_workspace_root_is_where_the_config_lives() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        write_config(dir.path(), "[agent]\nmodel = \"openai/gpt-4o\"\n");

        // Discovered by walking up from a subdirectory.
        let config = Config::load(&dir.path().join("src"), &Overrides::default()).unwrap();
        assert_eq!(
            config.workspace.root(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
        assert!(config.sessions_dir.ends_with(SESSIONS_DIR));
    }

    #[test]
    fn with_no_config_file_the_working_directory_is_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(dir.path(), &Overrides::default()).unwrap();
        assert!(config.source.is_none());
        assert_eq!(
            config.workspace.root(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
    }

    #[test]
    fn a_cli_profile_beats_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[policy]\nprofile = \"developer\"\n");
        let config = Config::load(
            dir.path(),
            &Overrides {
                profile: Some("reviewer".into()),
                ..Overrides::default()
            },
        )
        .unwrap();
        assert_eq!(config.profile, Profile::Reviewer);
    }

    #[test]
    fn an_unknown_profile_fails_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[policy]\nprofile = \"readnly\"\n");
        let err = Config::load(dir.path(), &Overrides::default()).unwrap_err();
        assert!(err.message().contains("unknown profile"), "{err}");
    }

    #[test]
    fn a_malformed_deny_glob_fails_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[workspace]\ndeny = [\"[\"]\n");
        let err = Config::load(dir.path(), &Overrides::default()).unwrap_err();
        assert!(err.message().contains("bad deny pattern"), "{err}");
    }

    #[test]
    fn a_model_without_a_provider_fails_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[agent]\nmodel = \"gpt-4o\"\n");
        let err = Config::load(dir.path(), &Overrides::default()).unwrap_err();
        assert!(err.message().contains("provider/model"), "{err}");
    }

    #[test]
    fn an_unknown_plugin_id_fails_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[plugins]\nenabled = [\"rivet.tool-filesystm\"]\n",
        );
        let err = Config::load(dir.path(), &Overrides::default()).unwrap_err();
        assert!(err.message().contains("unknown plugin id"), "{err}");
    }

    #[test]
    fn a_missing_explicit_config_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = Config::load(
            dir.path(),
            &Overrides {
                config_path: Some(dir.path().join("nope.toml")),
                ..Overrides::default()
            },
        )
        .unwrap_err();
        assert_eq!(err.kind(), rivet_core::error::ErrorKind::NotFound);
    }

    #[test]
    fn a_missing_credential_is_reported_with_the_variable_name() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[plugins.\"rivet.model-openai\"]\napi_key_env = \"RIVET_NOT_SET_ANYWHERE\"\n",
        );
        let config = Config::load(dir.path(), &Overrides::default()).unwrap();
        let err = config.check_credentials().unwrap_err();
        assert!(err.message().contains("RIVET_NOT_SET_ANYWHERE"), "{err}");
    }

    #[test]
    fn profiles_narrow_tool_scope_and_permissions() {
        assert!(Profile::Developer.writable());
        assert!(!Profile::ReadOnly.writable());
        assert!(!Profile::Production.writable());
        assert_eq!(
            Profile::Reviewer.tool_scope().unwrap(),
            ["read_file", "list_dir", "search"],
            "a reviewer that can edit the code is not a reviewer"
        );
        assert!(
            Profile::ReadOnly
                .permissions()
                .allows(&Permission::FsRead(FsScope::Workspace))
        );
        assert!(
            !Profile::ReadOnly
                .permissions()
                .allows(&Permission::FsWrite(FsScope::Workspace))
        );
    }

    #[test]
    fn only_ci_is_unattended_by_itself() {
        // `production` requires approval for everything, so calling it unattended would
        // -- once approvals exist -- auto-deny instead of asking.
        assert!(Profile::Ci.implies_unattended());
        assert!(!Profile::Production.implies_unattended());
        assert!(!Profile::Developer.implies_unattended());
    }

    #[test]
    fn headless_makes_any_profile_unattended() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[policy]\nprofile = \"developer\"\n");
        let config = Config::load(
            dir.path(),
            &Overrides {
                headless: true,
                ..Overrides::default()
            },
        )
        .unwrap();
        assert!(config.unattended);
    }
}
