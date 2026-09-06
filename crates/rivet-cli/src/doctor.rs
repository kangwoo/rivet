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
        "  profile     {} (narrows the agent's tool scope; policy enforcement is Phase 4)",
        config.profile.name()
    );
    println!("  unattended  {}", config.unattended);

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

    if config.inert.sandbox || config.inert.job || !config.inert.named_agents.is_empty() {
        println!("\nconfigured but not yet in force");
        if config.inert.sandbox {
            println!("  [sandbox]   read, but no confinement is applied until Phase 4");
        }
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
    let host = catalog::load(config).await?;
    healthy &= report_plugins(config, &host);
    host.shutdown();

    println!("\ntools offered to the model");
    let scope = config.profile.tool_scope();
    for name in host.registry.tool_names().await {
        let in_scope = scope.as_ref().is_none_or(|s| s.contains(&name));
        println!("  {} {name}", if in_scope { "+" } else { "-" });
    }

    Ok(healthy)
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
