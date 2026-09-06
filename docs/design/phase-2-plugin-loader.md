<!-- The design approved for Phase 2, kept because it is the "why" behind the code.
     Written before implementation; §8 records where the build departed from it and why.
     The body above §8 is not edited afterwards to look prescient. -->

# Phase 2 — Plugin loader: design

> 상태: 승인됨 (설계 리뷰 1라운드 PASS) · 구현: `docs/plan.md` Phase 2 · PR #1
> 이 문서는 **결정과 그 근거**를 남긴다. 무엇을 만들었는지는 커밋이, 계약의 현재 모습은
> [`plugin.md`](../plugin.md)가 말한다.
>
> 교차 Phase 파급이 있는 열린 질문은 [`architecture.md` §11](../architecture.md#11-열린-질문)로
> 승격했다. §7에 남은 것은 이 Phase 안에서 닫히는 것들이다.

> Scope: `docs/plan.md` §"Phase 2 — Plugin", items 2.1–2.7 and its five DoD lines.
> Written against the tree at `herdr/phase2-plugin-loader` (base `c2a6c37`).

---

## 1. Problem

Today a capability reaches the runtime only if `crates/rivet-cli/src/bootstrap.rs::build`
has a hardcoded `match` arm for its id, and only if that id also appears in one of two
string constants in `crates/rivet-cli/src/config.rs` (`IMPLEMENTED_PLUGINS`,
`PLANNED_PLUGINS`); the manifest each plugin advertises is hand-built in Rust
(`FilesystemPlugin::manifest_for`, `OpenAiPlugin::manifest_for`), its declared permissions
are never intersected with the active profile (`bootstrap.rs` passes
`config.profile.permissions()` straight through, so `manifest ∩ profile` exists only as a
test in `rivet-core`), the declared `capabilities` list is never enforced against what the
plugin actually registers, nothing ever calls `Plugin::unload`, and a single failing plugin
aborts the whole load with only the first failure named. Phase 2 replaces that with a real
loader in the currently-three-line `crates/rivet-plugin` crate: each plugin crate ships a
`rivet-plugin.toml` that is parsed and validated, the loader walks
`discover → validate → load → register → active`, rejects an incompatible ABI *before*
instantiating anything, computes `manifest ∩ profile` and hands the result to the plugin,
rolls back every partial registration on failure, supports unload/reload, and gives
`rivet plugin list|show|new` something real to print — all still in-process, still
statically linked, with no dynamic ABI (`docs/architecture.md` §8.4 forbids one until the
contract has been validated by a real plugin, and out-of-process is Phase 6).

---

## 2. Approach

### 2.1 The shape

```text
plugins/<crate>/rivet-plugin.toml            rivet.toml
   │ include_str!  (compile time)               │ [plugins].enabled + [plugins."<id>"]
   ▼                                            ▼
PluginSource { manifest_toml, construct, origin }        Config { selection, settings, profile }
   └──────────────┬───────────────────────────────────────────┘
                  ▼
        PluginLoader   (crates/rivet-plugin/src/loader.rs)
   ┌──────────┬──────────┬──────────────┬────────────┬────────┐
   │ discover │ validate │     load     │  register  │ active │
   │ parse    │ ABI +    │ construct +  │ Guarded    │ commit │
   │ dup ids  │ meet     │ Plugin::load │ Registry   │        │
   └──────────┴──────────┴──────────────┴─────┬──────┴────────┘
                                              ▼
                              rivet_runtime::Registry (ownership-tracked)
```

Five decisions carry the design:

**(a) The manifest is a file, embedded at compile time.** Each plugin crate gets a
`rivet-plugin.toml` at its crate root and exports it as
`pub const MANIFEST_TOML: &str = include_str!("../rivet-plugin.toml");`. The loader parses
that text with the same parser a Phase 6 on-disk manifest will use. Nothing about the
manifest lives in Rust literals any more, so `rivet plugin show` and the plugin's own
`manifest()` cannot disagree.

**(b) Discovery enumerates a catalog of `PluginSource`, not a directory.** In-process
plugins are linked, so the set of *loadable* plugins is fixed at compile time. Scanning a
directory would produce manifests nothing can instantiate — exactly the "middle category"
`docs/plan.md` says Phase 2 removes. `Origin` is an enum with one variant today
(`Builtin { crate_name }`) so Phase 6 can add `Manifest { path }` without moving the seam.

**(c) A plugin is constructed from its manifest alone; all other input arrives as JSON
config.** `PluginSource::construct` is `fn(PluginManifest) -> Arc<dyn Plugin>` — no config,
no host handles, infallible. Everything a plugin needs to decide comes from `ctx.config`
(the `[plugins."<id>"]` table) and `ctx.permissions`. That is the Phase 6 constraint
applied one phase early: a remote plugin gets bytes, not a `ModelId`. It is also what
removes the `match` — the CLI catalog is three `PluginSource` values, not three arms
keyed on strings.

**(d) The profile grant is met with the manifest inside the loader, and plugins read the
result.** `PluginContext.permissions = manifest.requested_permissions() ∩ profile_grant`
via the existing `PermissionSet::intersect`. `FilesystemPlugin` then registers `write_file`
only when `ctx.permissions.allows(&FsWrite(Workspace))`, which deletes
`config.profile.writable()` from the CLI and makes DoD 3 a property of the loader rather
than of a `match` arm.

**(e) Registration goes through a guard that enforces the declared capability list and
records what was actually registered.** `GuardedRegistry` wraps
`rivet_runtime::registry::ScopedRegistry`, implements `PluginRegistry`, refuses a
`register_*` call whose `CapabilityKind` is not in the manifest's `capabilities`, and
accumulates the observed names. `rivet-core` already promises this ("Registering into a
slot not declared here is a contract violation and the loader rejects it") and nothing
enforced it. The observed list — not the plugin's self-reported `PluginHandle` — is what
`rivet plugin list` and `rivet doctor` print.

**Zero `rivet-core` changes.** Every type Phase 2 needs (`PluginManifest`, `PluginContext`,
`PluginState`, `PluginEvent`, `CapabilityVersion::accepts`, `PermissionSet::intersect`,
`FsScope::meet`) is already there and already tested. Rule 4 of the task statement is
satisfied by not touching the crate at all.

### 2.2 Alternatives rejected

| Alternative | Rejected because |
|---|---|
| **Filesystem discovery** — scan `~/.rivet/plugins/*/rivet-plugin.toml` and `<workspace>/.rivet/plugins` | Nothing found there can be instantiated without a dynamic ABI, which `docs/architecture.md` §8.4 forbids until Phase 6. It would recreate the "enabled but not loadable" middle category that `docs/plan.md` says the loader removes. The `Origin` enum keeps the seam without building the half-feature. |
| **Keep `Plugin` instances in the catalog** (CLI builds `OpenAiPlugin::new(config.model)` and hands the loader `Arc<dyn Plugin>`) | Keeps the manual wiring the task names as the thing Phase 2 removes, and keeps a construction path that cannot survive a process boundary. Config-as-JSON is the Phase 6 shape. |
| **Manifest parsing in `rivet-core`** next to `PluginManifest` | `docs/plan.md` assigns 2.1 to `rivet-plugin`; `rivet-core` has no `toml` dependency and its module doc says it owns contracts, not parsing. |
| **`impl FromStr for CapabilityVersion` in `rivet-core`** so `abi_version = "0.1"` deserializes directly | A `rivet-core` change for a `rivet-plugin` convenience. The wire type in `rivet-plugin` does the string→version conversion in six lines and keeps core's JSON representation untouched. |
| **Derive the manifest straight onto `PluginManifest` with serde** | `PluginManifest` is flat (`id`, `name`, … at the top level) while the documented file has `[plugin]` plus `[[permissions]]`; `Permission`'s adjacently-tagged representation also cannot express `network_http` with an absent scope. A hand-written wire type is required anyway, and it buys far better error messages ("unknown permission `fs_reed`; expected one of …"). |
| **Abort the whole load on the first plugin failure** (today's behavior) | The operator gets one failure at a time out of a five-plugin config, and `rivet plugin list` / `rivet doctor` — the commands whose entire job is to report what is broken — die on the first problem instead of reporting it. The loader now records every failure and lets the caller decide fatality. |
| **Stub `Plugin` impls for `tool-shell` / `tool-git` / `policy-default` / `sandbox-local`** so the shipped example keeps loading six ids | A `git` plugin that registers no tools is a lie told to `rivet plugin list`. The example's `enabled` list is trimmed instead, with a comment pointing at Phase 4. |
| **Keep `ContextPlugin` inside `rivet-cli`** | It would be the one plugin whose manifest is not a file and whose construction is a special case, i.e. the last surviving arm of the `match`. It moves to `plugins/context-builtin` and becomes an ordinary catalog entry. |
| **Enforce `capabilities` inside `rivet-runtime::Registry`** | The registry does not see the manifest, and adding a declared-kinds parameter to `Registry::scoped` changes a Phase 0 contract for every embedder. The guard lives in the loader, which is where the manifest already is. |

---

## 3. Concrete changes

### 3.1 `crates/rivet-plugin` — the body of the phase (2.1–2.5)

`Cargo.toml`: add `tokio-util` (cancellation tokens), `futures-util` (`catch_unwind` on the
`load` future), `tracing`; dev-deps `tempfile`, `pretty_assertions`,
`tokio = { features = ["test-util", "macros"] }`.

- **`src/lib.rs`** — module doc stating what Phase 2 owns, what it deliberately does not
  (no dynamic loading, no directory scan), and the re-exports (`PluginLoader`,
  `PluginRecord`, `PluginSource`, `Origin`, `LoadReport`, `manifest::parse`).
- **`src/manifest.rs` (new)** — 2.1. `parse(text) -> Result<PluginManifest>` and
  `parse_from(text, &Origin)` (same, with the origin in every error message). Private wire
  types `RawManifest`/`RawPlugin`/`RawPermission` with `deny_unknown_fields`, plus
  `parse_abi_version` and `permission_from_raw`.
- **`src/source.rs` (new)** — `PluginSource`, `Origin`, and the `Construct` fn-pointer
  alias.
- **`src/guard.rs` (new)** — `GuardedRegistry`: the twelve `PluginRegistry` methods, each
  `require(CapabilityKind::X)?` then delegate then record the name.
- **`src/loader.rs` (new)** — 2.2/2.3/2.4/2.5. `PluginLoader`, `PluginRecord`,
  `LoadReport`, the state machine, rollback, per-instance cancellation child tokens, and
  publication of the four `PluginEvent` topics.
- **`tests/loader.rs` + `tests/support/mod.rs` (new)** — the DoD tests, driven by fake
  plugins declared inline (a tool plugin, a plugin that fails after two registrations, a
  plugin that panics, a plugin that registers into an undeclared slot, an ABI-0.99 spy
  whose `load` sets an `AtomicBool`).

### 3.2 `plugins/*` — repackaging (2.6)

- **`plugins/tool-filesystem/rivet-plugin.toml` (new)**; `src/lib.rs`: add
  `MANIFEST_TOML`; `FilesystemPlugin { manifest: PluginManifest }` with
  `new(manifest)`; `tools_for(&PermissionSet) -> Vec<Arc<dyn Tool>>` replacing
  `tools(self)`; delete `read_only()`, the `writable` field and `manifest_for`;
  `load` selects tools from `ctx.permissions`.
- **`plugins/model-openai/rivet-plugin.toml` (new)**; `src/lib.rs`: add `MANIFEST_TOML`;
  `OpenAiPlugin { manifest }`; the model id now comes from `ctx.config["agent"]["model"]`
  (see §4.4) instead of a constructor argument; delete `manifest_for`; `load` fails loudly
  when `NetworkHttp` was denied, before it builds an HTTP client.
- **`plugins/context-builtin/` (new crate)** — `Cargo.toml` (deps `rivet-core`,
  `rivet-runtime`, `async-trait`, `serde_json`, `tokio`), `rivet-plugin.toml`,
  `src/lib.rs` holding what is today `bootstrap::ContextPlugin`, reading its instructions
  from `ctx.config["agent"]["instructions"]`. Added to the workspace `members` list and to
  `[workspace.dependencies]`.
- `plugins/tool-shell`, `plugins/tool-git`, `plugins/policy-default`,
  `plugins/sandbox-local` are **untouched** — they have nothing to register until Phase 4.

### 3.3 `crates/rivet-cli` — catalog, commands, config (2.7)

- **`Cargo.toml`**: add `rivet-plugin`, `rivet-context-builtin`.
- **`src/bootstrap.rs` → `src/catalog.rs`** — `sources() -> Vec<PluginSource>` (three
  entries), `pub const CONTEXT_PLUGIN_ID`, and `load(&Config) -> Result<Host>` which
  creates the bus and registry, builds the loader, discovers, validates, loads the
  selection, and turns a non-empty `report.failed` into one error naming every failure.
  `ContextPlugin` and `fn build` are deleted.
- **`src/config.rs`** — delete `IMPLEMENTED_PLUGINS` and `PLANNED_PLUGINS` and
  `classify_plugins`; `Config.plugins` becomes `PluginSelection`; `deferred_plugins`
  disappears; add `Config::plugin_config(&PluginId) -> serde_json::Value` (file table plus
  the injected `agent` object, §4.4); `Profile::permissions()` gains
  `Permission::NetworkHttp(None)` (§5.7); `Profile::writable()` stays (it is what
  `permissions()` branches on) but nothing else consults it.
- **`src/plugin_cmd.rs` (new)** — `list`, `show`, `new`.
- **`src/main.rs`** — the three `PluginCommand` arms replace the "lands in Phase 2" stub;
  `list`/`show` do not load and therefore need no credentials.
- **`src/run.rs`** — `bootstrap::load` → `catalog::load`; `Loaded` → `Host`;
  `report_deferred` deleted; after the renderer drains, `host.loader.unload_all().await`
  so `Plugin::unload` finally has a caller on the normal path.
- **`src/doctor.rs`** — the `plugins` section prints one line per record (state, id,
  version, origin) plus its registrations and, for a failed one, the retained error;
  the "ships in a later phase; skipped" branch is deleted.
- **`templates/plugin/{Cargo.toml.tmpl,rivet-plugin.toml.tmpl,lib.rs.tmpl}` (new)** —
  `include_str!`-embedded, `{{id}}`/`{{name}}`/`{{crate_name}}` substituted with
  `str::replace`. No template-engine dependency.
- **`tests/plugin_cmd.rs` (new)**, `tests/e2e.rs` extended.

### 3.4 Docs and example (mandatory per the task)

- **`rivet.example.toml`** — `enabled` trimmed to `rivet.model-openai` +
  `rivet.tool-filesystem`; the comment explains that Phase 4 adds shell/git/policy/sandbox
  back and that an id that is not in the catalog is now a startup error, not a warning.
- **`docs/config.md`** §`[plugins]` — replace the three-id table and the "경고하고
  건너뛴다" paragraph with: every id either loads or is a typo; the id list comes from the
  catalog; `rivet.context-builtin` is still not switchable; document the reserved `agent`
  key inside `[plugins."<id>"]`; note in the profile table that every profile grants
  provider network access (§5.7).
- **`docs/plugin.md`** — add: where `rivet-plugin.toml` lives and that it is embedded with
  `include_str!`; the exact `scope` syntax per permission (§4.2 below); that registering
  into an undeclared slot is refused by the loader; that an `Interceptor` requires
  `capabilities = ["policy"]`; that an in-process plugin must also be added to the CLI
  catalog and the workspace, with the one line `rivet plugin new` prints.
- **`docs/security.md`** §8 — footnote the profile table: the `network` column is about
  *tool* egress, which nothing enforces until Phase 4; the model provider call is granted
  to every profile because otherwise no profile can run an agent.
- **`docs/plan.md`** — Phase 2 DoD checkboxes and the progress-tracking row, each with the
  test name that proves it. Anything not verified stays unchecked with a reason.

---

## 4. Data and interface shapes

### 4.1 `rivet-plugin.toml`

```toml
[plugin]
id           = "rivet.tool-filesystem"
name         = "Filesystem tools"
version      = "0.1.0"          # must equal the crate's CARGO_PKG_VERSION (tested)
abi_version  = "0.1"            # checked against rivet_core::ABI_VERSION before load
description  = "read_file, write_file, list_dir, search."
capabilities = ["tool"]         # CapabilityKind, snake_case; must be non-empty

[[permissions]]
permission = "fs_read"
scope      = "workspace"

[[permissions]]
permission = "fs_write"
scope      = "workspace"
```

Exactly the shape `docs/plugin.md` §2 already promises. Unknown keys in `[plugin]` or in a
`[[permissions]]` entry are rejected (`deny_unknown_fields`), as is an unknown top-level
table — a typo must not become a silently-absent permission.

### 4.2 `scope` grammar

| `permission` | `scope` | Meaning |
|---|---|---|
| `fs_read` / `fs_write` | `"workspace"` / `"anywhere"` | `FsScope::Workspace` / `Anywhere` |
| `fs_read` / `fs_write` | `{ subtree = "docs/api" }` | `FsScope::subtree(..)` — validated at parse |
| `network_http` | absent | any host (`NetworkHttp(None)`) |
| `network_http` | `["api.openai.com"]` | allowlist; an empty array is an error |
| `secrets_read` | `["DEEPSEEK_API_KEY"]` | required, non-empty |
| everything else | absent | scope present ⇒ error naming the permission |

`FsScope::subtree` is called rather than `FsScope::Subtree(..)` constructed, so
`{ subtree = "../../../etc" }` fails at parse. Letting it through would rely on
`FsScope::meet` returning `None` later; the manifest would then load with a silently empty
grant instead of telling the author their manifest is wrong.

### 4.3 `rivet-plugin` public API

```rust
// source.rs
pub type Construct = fn(PluginManifest) -> Arc<dyn Plugin>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Linked into this binary. Phase 6 adds `Manifest { path }`.
    Builtin { crate_name: &'static str },
}

#[derive(Clone, Copy, Debug)]
pub struct PluginSource {
    pub origin: Origin,
    manifest_toml: &'static str,
    construct: Construct,
}
impl PluginSource {
    pub const fn builtin(crate_name: &'static str, manifest_toml: &'static str,
                         construct: Construct) -> Self;
}

// manifest.rs
pub fn parse(text: &str) -> rivet_core::Result<PluginManifest>;
pub fn parse_from(text: &str, origin: &Origin) -> rivet_core::Result<PluginManifest>;

// loader.rs
#[derive(Clone, Debug)]
pub struct PluginRecord {
    pub id: PluginId,
    pub manifest: PluginManifest,
    pub origin: Origin,
    pub state: PluginState,                 // rivet_core::plugin::PluginState
    pub instance_id: Option<PluginInstanceId>,
    /// manifest ∩ profile, computed at VALIDATED.
    pub effective: PermissionSet,
    /// Permissions the manifest asked for that the profile removed. Drives `plugin show`.
    pub denied: Vec<Permission>,
    /// Observed through the guard, not self-reported. e.g. ["tool:read_file", ...]
    pub registered: Vec<String>,
    /// What `PluginHandle` claimed, kept for comparison, never trusted.
    pub claimed: Vec<String>,
    /// Retained for `rivet plugin list` after FAILED, per the PluginState doc.
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct LoadReport {
    pub loaded: Vec<PluginId>,
    pub failed: Vec<(PluginId, Error)>,
    pub registered: Vec<String>,
}

pub struct PluginLoader { /* Registry, Arc<dyn EventBus>, host_abi, profile grant,
                             root CancellationToken, Vec<PluginRecord>,
                             HashMap<PluginId, Instance> */ }

impl PluginLoader {
    pub fn new(registry: Registry, events: Arc<dyn EventBus>,
               host_abi: CapabilityVersion, profile_grant: PermissionSet) -> Self;

    /// Parse every manifest. Duplicate ids are an error naming both origins.
    /// Leaves each record DISCOVERED. Publishes `plugin.discovered`.
    pub fn discover(&mut self, sources: &[PluginSource]) -> rivet_core::Result<()>;

    /// ABI check + permission meet. VALIDATED or FAILED. Nothing is constructed here.
    pub fn validate(&mut self);

    pub async fn load(&mut self, id: &PluginId, config: Value) -> rivet_core::Result<()>;
    pub async fn load_selected(&mut self, ids: &[PluginId],
                               config_for: &dyn Fn(&PluginId) -> Value) -> LoadReport;
    pub async fn unload(&mut self, id: &PluginId) -> rivet_core::Result<()>;
    pub async fn unload_all(&mut self);
    /// Cancel every plugin's shutdown token without unloading. Ctrl-C path.
    pub fn shutdown(&self);

    pub fn records(&self) -> &[PluginRecord];
    pub fn record(&self, id: &PluginId) -> Option<&PluginRecord>;
}
```

State transitions, matching `docs/plugin.md` §7 and `PluginState`:

```text
discover()  -> DISCOVERED
validate()  -> VALIDATED | FAILED            (ABI, permission meet; nothing constructed)
load()      -> construct -> Plugin::load through GuardedRegistry
               ok  -> LOADED
               err -> unregister_all + cancel child token -> FAILED (error retained)
load_selected() commits the batch: every LOADED record becomes ACTIVE.
unload()    -> UNLOADING -> unregister_all -> cancel child token
               -> Plugin::unload -> UNLOADED  (an Err from unload is retained, not fatal:
                                               the capabilities are already gone)
```

`ACTIVE` is reserved for "the host accepted the whole batch", so a record left at `LOADED`
after `load_selected` is visibly one the host is about to tear down. A record may return to
`VALIDATED` and be loaded again (hot reload, DoD 5) with a fresh `PluginInstanceId`.

### 4.4 Host-supplied config

`PluginContext.config` is the `[plugins."<id>"]` table as JSON, with one object merged in
by the host for **every** plugin, uniformly:

```json
{ "agent": { "model": "deepseek/deepseek-chat", "instructions": "…" } }
```

`agent` is a reserved key inside a plugin table: if a config file sets it, startup fails
naming the file and the key. This is what lets `PluginSource::construct` take no arguments
— `rivet.model-openai` reads `agent.model`, `rivet.context-builtin` reads
`agent.instructions` — without the CLI keeping a per-id mapping, and it is the same shape
that survives a process boundary in Phase 6. `OpenAiConfig` ignores unknown keys already
(`#[serde(default)]`, no `deny_unknown_fields`), so the injected object costs it nothing.

### 4.5 CLI surface

```text
$ rivet plugin list                       # discover + validate only. No credentials needed.
STATE      ID                       VERSION  ABI   CAPABILITIES  ORIGIN
VALIDATED  rivet.model-openai       0.1.0    0.1   model         builtin(rivet-model-openai)
VALIDATED  rivet.tool-filesystem    0.1.0    0.1   tool          builtin(rivet-tool-filesystem)
VALIDATED  rivet.context-builtin    0.1.0    0.1   context_provider  builtin(rivet-context-builtin)
  run `rivet doctor` to load them and see what each registers

$ rivet plugin show rivet.tool-filesystem --profile readonly
rivet.tool-filesystem  "Filesystem tools" 0.1.0
  abi          0.1 (host 0.1) ok
  origin       builtin(rivet-tool-filesystem)
  capabilities tool
  permissions  requested                  effective
               fs_read(workspace)         granted
               fs_write(workspace)        removed by profile `readonly`

$ rivet plugin new acme.tool-lint
created acme-tool-lint/{Cargo.toml,rivet-plugin.toml,src/lib.rs}
next: add "acme-tool-lint" to the workspace members, then add one line to
      crates/rivet-cli/src/catalog.rs:
        PluginSource::builtin("acme-tool-lint", acme_tool_lint::MANIFEST_TOML,
                              |m| Arc::new(acme_tool_lint::LintPlugin::new(m))),
      (in-process plugins are linked; out-of-process loading is Phase 6)
```

`plugin list`/`show` deliberately stop at `VALIDATED`: they need no API key and have no
side effects. Today `rivet plugin list` calls `bootstrap::load` and therefore *fails* on a
machine with no provider key — a bug this design removes rather than preserves.
`rivet plugin new` writes into `<cwd>/<id with dots as dashes>`, refuses to write into an
existing directory, and validates the id through `PluginId::new` before touching disk.

---

## 5. Failure modes

| # | Failure | Handling |
|---|---|---|
| 1 | Manifest is not valid TOML, or a field is missing/misspelled/unknown | `Error::plugin` naming the origin and the `toml` span; record `FAILED` at discover. Nothing is constructed. |
| 2 | `id` is not `namespace.name` | `PluginId::new`'s existing message, prefixed with the origin. |
| 3 | Two sources declare the same id | `discover` returns `Err` naming **both** origins. This is a build-time mistake in the catalog, so it is fatal rather than recorded. |
| 4 | `abi_version` incompatible (2.3) | `validate` records `FAILED` with "built against 0.99, host implements 0.1". `construct` is never called, so `Plugin::load` never runs. Tested with a spy. |
| 5 | Manifest asks for an escaping subtree | Rejected at parse (§4.2), not silently met away. |
| 6 | Profile removes a permission the plugin needs | The plugin decides at `load`: `model-openai` fails loudly (`Err`) because a model with no network is useless; `tool-filesystem` degrades (registers three tools instead of four) because that is Phase 1's documented behavior for `readonly`. Both idioms are in `docs/plugin.md` §4.2; the design keeps both and says which is which. |
| 7 | Plugin registers into a slot it did not declare | `GuardedRegistry` returns `Err` naming the slot, the manifest's list and the id; the load fails and rolls back (5.8). |
| 8 | Name collision (DoD 4) | `Registry`'s existing message already names both plugins; the loader propagates it verbatim, marks the *second* plugin `FAILED`, and rolls it back. The first plugin keeps its registration. |
| 9 | `load` returns `Err` after partial registration (DoD 1) | `registry.unregister_all(instance)` + cancel that instance's child token; record `FAILED` with the error and an empty `registered`. |
| 10 | `load` panics | The future is run inside `AssertUnwindSafe(..).catch_unwind()`; the panic becomes `Error::plugin("… panicked: <payload>")` and takes path 9. The record stays `FAILED` and is not retried in this process. Caveat: a panic in a task the plugin itself spawned is not caught, and neither is anything under `panic = "abort"`. |
| 11 | `unload` returns `Err` | Capabilities are already unregistered, so the state is `UNLOADED` with the error retained and reported. Reporting it as `FAILED` would suggest the registry is dirty when it is not. |
| 12 | Loading an id that is already `ACTIVE` | `Err` naming the state. Reload is `unload` then `load`, never an implicit second instance. |
| 13 | `enabled` names an id not in the catalog | `Err` listing the catalog ids — exit code 2, the same class as an unknown profile. The `PLANNED_PLUGINS` warning path is gone: every id loads or is a typo. |
| 14 | A `[plugins."<id>"]` table for an id that is not enabled | Not fatal (people comment ids out and keep the table). `rivet doctor` reports it as an orphan. A malformed id as the *key* is still fatal. |
| 15 | One plugin fails while others loaded | Every plugin is attempted; `LoadReport.failed` collects them all. `rivet run` turns a non-empty `failed` into one error naming every failure, then calls `unload_all` so a half-loaded process does not linger. `doctor`/`plugin list` print and continue. |
| 16 | Unloading one plugin must not stop another's background work | Each instance gets `root.child_token()`; `unload` cancels only that child, `shutdown` cancels the root. Today's single shared token would stop every plugin's background work when one unloads. |

---

## 6. Test strategy

Everything below is offline (`cargo test --workspace --offline`). Baseline to hold or
raise: **385 passed · 0 failed · 1 ignored · clippy 0**. The two `bootstrap.rs` tests that
disappear with the `match` (`every_implemented_id_can_actually_be_built`,
`the_context_plugin_id_is_valid_and_matches_the_config_list`) are replaced by catalog
equivalents, so the count does not fall.

**`rivet-plugin` manifest unit tests** — every permission variant round-trips from TOML
(both `network_http` forms, `secrets_read`, both fs scopes plus a subtree, and the six
scope-less ones); escaping subtree rejected; unknown permission name, unknown capability
kind, unknown field, missing field, empty `capabilities`, and malformed `abi_version`
(`"0"`, `"x.y"`, `"0.1.2"`) each produce an error naming the offending token.

**`rivet-plugin` loader integration tests** — one per DoD line, named so `docs/plan.md` can
cite them:

| DoD (verbatim) | Test |
|---|---|
| 부분 등록 후 실패한 plugin이 **아무것도** 남기지 않음 | `a_plugin_that_fails_after_registering_leaves_nothing` — registers two tools, returns `Err`; asserts `registry.tool_names()` empty, the other plugin's tools intact, record `FAILED`, `registered` empty. Sibling: `…_that_panics_…`. |
| ABI 불일치 plugin이 등록 시도 전에 거부됨 | `an_incompatible_abi_is_rejected_before_load_is_called` — spy plugin sets an `AtomicBool` in `load`; asserts the flag is false, the state is `FAILED`, and the registry is empty. |
| `readonly` 프로파일이 쓰기 plugin의 쓰기 권한을 실제로 제거 | `a_readonly_profile_strips_write_permission` (loader level: `effective` lacks `FsWrite`, `denied` names it) + `a_readonly_profile_leaves_write_file_unregistered` in `tool-filesystem`. |
| 이름 충돌 시 양쪽 plugin 이름이 에러에 나옴 | `a_name_collision_names_both_plugins` — asserts both ids appear in the error, the first keeps the entry, the second is rolled back. |
| unload 후 같은 이름 재등록 성공 | `a_plugin_reloads_under_the_same_name` — load → unload → load; asserts a new `PluginInstanceId` and that the tool resolves again. |

Plus: the undeclared-slot guard; duplicate ids across sources; loading an unknown id;
unloading an id that is not loaded; the four `plugin.*` events arriving at a subscriber;
and that unloading plugin A does not cancel plugin B's token.

**Plugin crate tests** — each of the three shipping crates asserts (dev-dependency on
`rivet-plugin`) that its `rivet-plugin.toml` parses, that its `version` equals
`env!("CARGO_PKG_VERSION")`, that its id equals the crate's `PLUGIN_ID` const, and that its
`capabilities` covers everything it registers. `tool-filesystem` keeps its existing
`every_spec_passes_the_runtimes_schema_check`.

**CLI tests** — catalog ids are unique and every source parses; the shipped
`rivet.example.toml` still loads *and* every id it enables resolves in the catalog (this is
the test that keeps the example honest); an unknown id fails with exit 2 and lists the
catalog; `rivet plugin list` exits 0 with **no** API key set (today's regression);
`plugin show` prints requested-vs-effective and marks `fs_write` removed under
`--profile readonly`; `plugin show unknown.id` exits 2; `plugin new` scaffolds into a
tempdir, its generated manifest parses through `rivet_plugin::manifest::parse`, and a
second run against the same directory refuses.

**e2e (`crates/rivet-cli/tests/e2e.rs`)** — the existing suite must stay green unchanged
(it is the proof that repackaging did not break the run path); add
`readonly_does_not_offer_write_file`, asserting through `rivet doctor` output under
`--profile readonly` and `--profile developer`.

**Gates** — `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -D
warnings` (note `missing_debug_implementations` is `warn` + `-D warnings`: every new public
type needs `Debug`, and `PluginLoader` needs a manual impl because it holds
`HashMap<PluginId, Instance>`), `cargo test --workspace`, `RUSTDOCFLAGS="-D warnings" cargo
doc --workspace --no-deps`.

---

## 7. Open questions

Listed rather than assumed. Each has a provisional answer used by this design, marked
**[assumed]**, so the build is not blocked — but each is a decision the task statement does
not settle.

1. **Provider egress vs. tool egress.** `Profile::permissions()` grants no `NetworkHttp`
   today, so the moment `manifest ∩ profile` is real, `rivet.model-openai` intersects to an
   empty grant and no profile can run an agent. `docs/security.md` §8 says `readonly` and
   `reviewer` have `network ✗` — yet its own example is
   `rivet --profile readonly "왜 이 테스트가 실패하지?"`, which requires a provider call.
   **[assumed]** every profile grants `NetworkHttp(None)` in Phase 2, with a footnote in
   `docs/security.md` that the table's column is about tool egress and that
   `production`'s allowlist arrives with the Phase 4 sandbox. If the intent is that a
   profile can forbid the provider call, the vocabulary needs a separate permission and
   that is a `rivet-core` change.
2. **`Interceptor` has no `CapabilityKind`.** The enum has no `Interceptor` variant, so the
   guard cannot map `register_interceptor` to a declared slot. **[assumed]** it requires
   `capabilities = ["policy"]`, documented in `docs/plugin.md`. The alternative — adding a
   variant to `CapabilityKind` — is an additive `rivet-core` change that widens the closed
   vocabulary, which Phase 2's scope boundary says to avoid. Worth revisiting in Phase 4
   when interceptors actually run.
3. **`Permission::EventsSubscribe` is in no profile grant.** Nothing in Phase 2 requests
   it, so **[assumed]** leave it alone — but Phase 3's `telemetry.log` plugin will
   intersect to an empty grant on day one unless a profile grants it.
4. **What `rivet plugin new` should emit for `rivet-core`.** Nothing is published to
   crates.io, so a generated `rivet-core = "0.1"` cannot resolve, and a path dependency is
   only correct inside this repository. **[assumed]** emit the version dependency with the
   path alternative in a comment, and do not compile the scaffold in CI. If the scaffold is
   meant to be verifiable, either `rivet-core` must be published or `plugin new` needs to
   detect the in-workspace case.
5. **Whether a plugin may be listed in `enabled` more than once, or ordered.** Load order
   is currently the order of `enabled` (dedup, first occurrence wins). Registration order
   is not supposed to matter — `Registry` rejects collisions and `interceptors()` sorts by
   priority — but nothing states it. **[assumed]** dedup, preserve first-seen order,
   document that order is not a contract.
6. **Should `rivet plugin list` have a `--json` mode?** `PluginRecord` is one derive away
   from it and it is the natural input for tooling. Not in the 2.x table, so
   **[assumed]** out of scope for this phase.
7. **`Config::api_key_env()` still reaches into `[plugins."rivet.model-openai"]` from the
   CLI** to pre-check credentials before a session exists. That is a host knowing a
   specific plugin's config key — the thing this phase otherwise removes. **[assumed]**
   left as-is (it is Phase 1 behavior, it produces the better error, and the plugin also
   checks at load), but it is the one hardcoded plugin id left in the CLI after Phase 2 and
   a reviewer may reasonably want it named as debt in `docs/config.md`.
8. **Whether unloading should be attempted after a failed batch in `rivet run`.** This
   design calls `unload_all` before returning the error, which means a plugin's `unload`
   runs after a sibling's `load` failed. `docs/plugin.md` does not say whether `unload` may
   be called on a plugin that loaded successfully but was never `ACTIVE`. **[assumed]**
   yes, and `unload` is documented as "stop what you started", which is well-defined in
   that state.

---

## 8. 구현이 설계에서 벗어난 곳

설계 리뷰를 통과한 뒤 구현하면서 갈라진 지점 셋, 그리고 PR 리뷰 2라운드에서 되돌아와
고친 둘. 나머지 §3 항목은 쓰인 대로 만들어졌다.

Three from the build, plus two the second PR review sent back. Everything else in §3 was
built as written.

---

### 1. A manifest that does not parse is fatal at `discover`, not a `FAILED` record

**Design** — §5 failure row 1: "Manifest is not valid TOML, or a field is
missing/misspelled/unknown → `Error::plugin` naming the origin and the `toml` span;
**record `FAILED` at discover**."

**What I hit** — a `PluginRecord` is keyed by `PluginId`, and the id comes *out of* the
manifest. A manifest that failed to parse has no id, so there is nothing to file the record
under. Making `PluginRecord.id` an `Option<PluginId>` would poison every consumer
(`record(&id)`, the `plugin list` table, `select`, `doctor`) to serve one case.

**What I did** — `PluginLoader::discover` returns `Err` naming the origin, exactly like a
duplicate id does. Both are mistakes in the *host's catalog* rather than in an operator's
config, and for `Origin::Builtin` the manifest is `include_str!`-embedded, so a parse
failure is a build bug that should be loud rather than a row in a table.

Everything the design wanted from the error is still there: the origin is named
(`builtin(acme-tool-lint): …`) and the `toml` span rides along as a cause. Phase 6, whose
on-disk manifests can fail per-file at runtime, is where a partial-record shape earns its
keep — and `Origin` is already the enum that will carry it.

Tests: `two_sources_claiming_one_id_name_both_origins`,
`manifest::tests::the_origin_is_named_in_every_error`.

---

### 2. `Origin` and `PluginSource` derive `Clone`, not `Copy`

Taken from review-1's non-blocking finding on `design.md:257,263`. The Phase 6 variant the
design plans (`Manifest { path }`) carries a `PathBuf` and would force both off `Copy`,
which is churn at exactly the seam the enum exists to protect. `Clone` costs nothing here —
the loader clones a source once per `load`.

---

### 3. `PluginRecord.claimed` is compared, not just kept

The design (§4.3) keeps `claimed` "for comparison, never trusted" but never compares it;
review-1 flagged that as a field that either earns its place or goes. It earns it:
`PluginRecord::claim_matches_reality()` sorts both lists and `rivet doctor` prints

```
    ! this plugin reported [...] but registered [...]
```

and marks the run unhealthy. A plugin whose `PluginHandle` overstates what it registered is
precisely what the guard exists to catch, and `doctor` is where an operator would look.

Covered by `what_a_plugin_claims_is_checked_against_what_it_registered` (loader) and the
negative assertion in `doctor_prints_what_each_plugin_actually_registered` (e2e).

---

### 4. The guard closes: registration is legal only inside `load`

**Design** — §4.3 and §5 give the guard two jobs, refusing undeclared slots and recording
what was registered, and describe the failure path as `unregister_all(instance_id)` plus a
token cancel. It never asks how long the handle stays live.

**Built (review-2)** — it stays live forever, because that is what `PluginContext` is:
`Clone`, with an `Arc` registry. Review-2 reproduced both consequences. A plugin whose
`load` spawns a task and then returns `Err` gets that task's registration *after* the
rollback, owned by an instance id no `unload` will ever name and invisible to
`record.registered` — so `rivet doctor`, `rivet plugin list` and `claim_matches_reality`
are all blind to it and nothing in the process can remove it. And a plugin that registers
from `unload` — which runs *after* `unregister_all` — holds its own name against its next
load, defeating DoD 5 while the record reads `UNLOADED` with an empty registration list.

`GuardedRegistry` now has a registration window that the loader closes after `load`
returns (both paths) and again before `Plugin::unload` runs. A `register_*` after that
fails with an error naming the plugin, and logs at `warn` — the caller is a task the loader
cannot see and is free to drop the `Err`.

The window is an `RwLock<bool>` rather than the `AtomicBool` the review suggested: every
`register_*` holds the read side across its whole check-delegate-record sequence, so `seal`
waits out the registrations already in flight. With a flag, one that had passed the check
could still land after the loader read `observed()` — a capability accepted by the registry
and missing from `record.registered`, which is the same bug in a smaller window.

§5 row 10 says a panic inside a task the plugin spawned is not caught. That is still true,
and is now the *only* thing such a task can do that outlives the phase's guarantees.

Covered by `a_registration_from_a_task_outliving_a_failed_load_is_refused` and
`a_plugin_that_registers_from_unload_does_not_break_its_own_reload`; both fail without the
seal.

---

### 5. `load_selected` promotes only its own batch

**Design** — §4.3: "`load_selected()` commits the batch: every LOADED record becomes
ACTIVE."

**Built (review-2)** — taken literally, and *every* `LOADED` record means every record in
the loader, not every record in the batch. A first batch that left a plugin at `LOADED`
(because a sibling failed, so the host is about to tear it down) has that plugin silently
promoted to `ACTIVE` by any later batch that succeeds. Both of the meanings §4.3 gives the
two states are then false for it. The promotion is now filtered by `report.loaded`.

`catalog::load` calls `load_selected` exactly once, so no in-tree caller reaches this; it
is a public API and hot reload is the Phase 2 feature that would. Covered by
`a_second_batch_does_not_promote_the_records_of_the_first`.

---

### Not deviations, but decisions the design left open

- **`load_selected` promotes `LOADED → ACTIVE` only when `report.failed` is empty.** This
  is the `design.md:341` vs `:347` contradiction review-1 named; the second reading is the
  one that makes both sentences true, and it is what makes a record left at `LOADED`
  meaningful ("the host is about to tear this down"). §8-5 narrows *which* records that
  promotes. Tested by
  `a_batch_becomes_active_only_when_every_plugin_loaded` and
  `a_batch_with_nothing_failing_commits_to_active`.

- **An empty or absent `[plugins].enabled` resolves to the whole catalog.** The other item
  review-1 asked for. `PluginSelection::All` is what an absent or empty list becomes, and
  the host resolves it against its catalog. Pinned by three tests, one per layer:
  `no_enabled_list_means_every_plugin_the_build_provides` (config),
  `an_absent_enabled_list_selects_the_whole_catalog` (catalog), and
  `with_no_config_file_every_plugin_the_build_provides_is_loaded` (e2e, which is the one
  that would actually have caught a silent regression — every other e2e workspace writes an
  explicit list).

- **`load` accepts a record in `VALIDATED` *or* `UNLOADED`.** §4.3 says both "`unload()` →
  `UNLOADED`" and "a record may return to `VALIDATED` and be loaded again". Accepting both
  states satisfies both sentences without a spurious transition.

- **No timeout on `Plugin::load` / `unload`.** review-1 noted the failure table is silent on
  a *hanging* plugin. I left it silent rather than inventing a budget the design did not
  specify; it is written down as unverified in `docs/plan.md` and in the build summary.
