//! `rivet doctor`: what this configuration actually resolved to.
//!
//! The point is to answer questions an operator otherwise has to guess at. Which
//! `rivet.toml` won? Which directory is the workspace, and therefore what is fenced in?
//! Do the deny globs match the files the operator thinks they match? Is the API key
//! variable set? Which plugins are real and which are placeholders?

use std::path::Path;

use crate::bootstrap;
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
    let loaded = bootstrap::load(config).await?;
    for entry in &loaded.registered {
        println!("  + {entry}");
    }
    for id in &loaded.deferred {
        println!("  · {id} (ships in a later phase; skipped)");
    }
    loaded.shutdown();

    println!("\ntools offered to the model");
    let scope = config.profile.tool_scope();
    for name in loaded.registry.tool_names().await {
        let in_scope = scope.as_ref().is_none_or(|s| s.contains(&name));
        println!("  {} {name}", if in_scope { "+" } else { "-" });
    }

    Ok(healthy)
}
