//! `rivet doctor`: what this configuration actually resolved to.
//!
//! The point is to answer questions an operator otherwise has to guess at. Which
//! `rivet.toml` won? Which directory is the workspace, and therefore what is fenced in?
//! Do the deny globs match the files the operator thinks they match? Is the API key
//! variable set? Which plugins loaded, and what did each one actually register?

use std::path::Path;

use rivet_plugin::loader::state_label;

use crate::catalog;
use crate::config::Config;

/// Print the resolved configuration and check what can be checked offline.
///
/// # Errors
/// Plugin load failures. A missing credential is reported, not raised: an operator running
/// `doctor` is trying to find that out.
pub async fn run(config: &Config) -> rivet_core::Result<bool> {
    let mut healthy = true;

    println!("configuration");
    println!(
        "  file        {}",
        config.source.as_ref().map_or_else(
            || "(none; built-in defaults)".to_string(),
            |p| p.display().to_string()
        )
    );
    println!("  workspace   {}", config.workspace.root().display());
    println!("  sessions    {}", config.sessions_dir.display());
    println!("  model       {}", config.model);
    println!(
        "  profile     {} (narrows the agent's tool scope, and computes the grant \
         the policy chain enforces)",
        config.profile.name()
    );
    println!(
        "  unattended  {} (`--headless` or the `ci` profile; a non-terminal stdin also \
         leaves nobody to ask)",
        config.unattended
    );

    println!("\nlimits");
    println!("  turns       {}", config.limits.max_turns);
    println!("  duration    {}ms", config.limits.max_duration_ms);
    println!("  tokens      {}", config.limits.max_total_tokens);
    println!("  context     {}", config.limits.max_context_tokens);
    println!(
        "  tool errors {}",
        config.limits.max_consecutive_tool_errors
    );

    println!("\ndeny list");
    if config.workspace.deny_patterns().is_empty() {
        println!("  (empty) — nothing inside the workspace is protected");
        healthy = false;
    } else {
        for pattern in config.workspace.deny_patterns() {
            println!("  {pattern}");
        }
        // The globs are compiled; show that they bite on the paths they advertise.
        for probe in [".env", "sub/.env", ".git/config", "keys/server.pem"] {
            let blocked = config.workspace.resolve(Path::new(probe)).is_err();
            println!(
                "  probe {probe:<20} {}",
                if blocked { "blocked" } else { "allowed" }
            );
        }
    }

    if config.inert.job || !config.inert.named_agents.is_empty() {
        println!("\nconfigured but not yet in force");
        if config.inert.job {
            println!("  [job]      read, but the job runtime lands in Phase 5");
        }
        if !config.inert.named_agents.is_empty() {
            println!(
                "  [agents.*]  {} declared; Phase 1 has no way to select one",
                config.inert.named_agents.join(", ")
            );
        }
    }

    println!("\ncredentials");
    match config.check_credentials() {
        Ok(()) => println!("  {} is set", config.api_key_env()),
        Err(error) => {
            println!("  {error}");
            healthy = false;
        }
    }

    println!("\nplugins");
    // `doctor` makes its own bus and attaches nothing to it. It diagnoses a runtime rather
    // than starting one, so it does not publish `runtime.started` -- a topic that meant
    // "a run began" and also "somebody ran doctor" would mean neither.
    let host = catalog::load(config, rivet_runtime::BroadcastBus::new()).await?;
    healthy &= report_plugins(config, &host);
    healthy &= report_sandbox(config, &host).await;
    host.shutdown();

    println!("\ntools offered to the model");
    let scope = config.profile.tool_scope();
    for name in host.registry.tool_names().await {
        let in_scope = scope.as_ref().is_none_or(|s| s.contains(&name));
        println!("  {} {name}", if in_scope { "+" } else { "-" });
    }

    Ok(healthy)
}

/// The confinement processes will run under, and whether anything can start one.
///
/// # "Missing" is not the same as "wrong"
///
/// The provider name is *always* printed, because an operator asked for it. But
/// `healthy = false` needs **both** of:
///
/// 1. a plugin that actually loaded still holds `process_spawn`, and
/// 2. nothing is registered under the resolved provider name.
///
/// Condition 1 is what keeps this from being the same mistake as refusing at pipeline step
/// 7. Without it, `--profile production` could never exit 0 — that profile grants no
/// `process_spawn`, so `sandbox-local` registers nothing there **by design** — and any
/// existing `rivet.toml` with a hand-written `enabled` list would go from exit 0 to exit 2
/// on upgrade alone. Neither of those configurations can start a process, so neither of
/// them is missing anything.
///
/// `instance_id.is_some()` is the whole of "actually loaded", and it is not decoration.
/// `catalog::load` runs `validate()` over the **entire catalog**, and that is where
/// `record.effective` is computed — so an unloaded `sandbox-local` still has
/// `process_spawn` in its effective grant under `developer`. Counting records without the
/// filter would make condition 1 true for every developer profile, including the ones with
/// no process-capable plugin loaded at all. The same filter already appears a few lines
/// below, for orphaned `[plugins."<id>"]` tables.
///
/// The material is `record.effective`, not a list of tool names. Asking "is `shell`
/// registered" would put a specific plugin's tool names back into the host — the coupling
/// Phase 2 removed, and the one `Config::api_key_env` is the last of.
async fn report_sandbox(config: &Config, host: &catalog::Host) -> bool {
    let registered = host.registry.sandbox(&config.sandbox_provider).await;
    let can_spawn: Vec<&str> = host
        .loader
        .records()
        .iter()
        .filter(|record| record.instance_id.is_some())
        .filter(|record| {
            record
                .effective
                .contains(&rivet_core::capability::Permission::ProcessSpawn)
        })
        .map(|record| record.id.as_str())
        .collect();

    println!("\nsandbox");
    println!(
        "  provider    {} ({})",
        config.sandbox_provider,
        if registered.is_some() {
            "registered"
        } else {
            "not registered by any loaded plugin"
        }
    );
    if let Some(sandbox) = &registered {
        let guarantees = sandbox.guarantees();
        println!(
            "  isolates    filesystem {} · network {} · processes {}",
            yes_no(guarantees.filesystem_isolation),
            yes_no(guarantees.network_isolation),
            yes_no(guarantees.process_isolation)
        );
    }
    if can_spawn.is_empty() {
        println!("  no loaded plugin may start a process, so nothing needs one");
        return true;
    }
    println!("  may spawn   {}", can_spawn.join(", "));
    if registered.is_some() {
        return true;
    }
    println!(
        "  ! `{}` is not registered, so every process those plugins try to start \
         will be refused",
        config.sandbox_provider
    );
    false
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// One block per plugin: what it is, and what it actually put in the registry.
///
/// Returns whether everything looked right, which is not the same as "it loaded": a
/// plugin whose `PluginHandle` overstates what it registered loaded fine and is still
/// something an operator needs to know about.
fn report_plugins(config: &Config, host: &catalog::Host) -> bool {
    let mut healthy = true;
    for record in host.loader.records() {
        println!(
            "  {:<10} {:<24} {:<8} {}",
            state_label(record.state),
            record.id.as_str(),
            record.manifest.version,
            record.origin
        );
        for entry in &record.registered {
            println!("    + {entry}");
        }
        // What a plugin registered is observed through the loader's guard; its
        // `PluginHandle` is only a claim. A divergence is worth surfacing -- it is
        // exactly the kind of thing the guard exists to catch.
        if !record.claim_matches_reality() {
            println!(
                "    ! this plugin reported {:?} but registered {:?}",
                record.claimed, record.registered
            );
            healthy = false;
        }
        if let Some(error) = &record.error {
            println!("    ! {error}");
        }
    }

    // A `[plugins."<id>"]` table for something nothing loaded is not fatal -- people
    // comment ids out and keep the table -- but it is silently doing nothing.
    let loaded: Vec<&str> = host
        .loader
        .records()
        .iter()
        .filter(|record| record.instance_id.is_some())
        .map(|record| record.id.as_str())
        .collect();
    for id in config.plugin_settings.keys() {
        if !loaded.contains(&id.as_str()) {
            println!("  \u{b7} `[plugins.\"{id}\"]` is configured but `{id}` is not loaded");
        }
    }
    healthy
}
