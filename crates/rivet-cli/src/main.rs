//! The `rivet` binary.
//!
//! The CLI is a *client of the runtime*, on exactly the same footing as the TUI: it
//! subscribes to events and renders them. Nothing here reaches into the agent loop.
//!
//! Phase 0 shipped the command surface; Phase 1 connects it to a runtime that exists. The
//! surface itself is unchanged, because it was reviewed as the shape of the UX.

mod bootstrap;
mod config;
mod doctor;
mod exit;
mod render;
mod run;
mod session_cmd;
mod signals;

use clap::{Parser, Subcommand};

use crate::config::{Config, Overrides};
use crate::run::Output;

#[derive(Debug, Parser)]
#[command(
    name = "rivet",
    version,
    about = "The model thinks. The runtime acts. Plugins define behavior.",
    long_about = None
)]
struct Cli {
    /// A prompt to run directly: `rivet "fix the failing test"`.
    prompt: Option<String>,

    /// Run without a TUI and without approval prompts. Anything that would ask a human
    /// is denied instead of hanging. Intended for CI.
    #[arg(long, global = true)]
    headless: bool,

    /// Emit newline-delimited JSON events on stdout instead of rendering.
    ///
    /// For observation only: the event bus is lossy by design, so this stream cannot
    /// reconstruct a session. Use `rivet session show --json` for that.
    #[arg(long, global = true, conflicts_with = "headless")]
    jsonl: bool,

    /// Override the configured policy profile.
    ///
    /// In Phase 1 a profile narrows which tools the agent is offered. It is not policy
    /// enforcement; that arrives in Phase 4.
    #[arg(long, global = true, value_name = "PROFILE")]
    profile: Option<String>,

    /// Path to `rivet.toml`. Defaults to the nearest one up the tree.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a prompt, optionally as a tracked job.
    Run {
        prompt: String,
        /// Create a job and drive it through the configured workflow.
        #[arg(long)]
        job: bool,
    },
    /// Resume a previous session.
    Resume { session: String },
    /// Inspect sessions.
    #[command(subcommand)]
    Session(SessionCommand),
    /// Inspect and drive jobs.
    #[command(subcommand)]
    Job(JobCommand),
    /// Inspect plugins.
    #[command(subcommand)]
    Plugin(PluginCommand),
    /// Check configuration, plugin health, and provider credentials.
    Doctor,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    List,
    Show {
        id: String,
        /// Print the durable log as JSON, one event per line.
        #[arg(long)]
        json: bool,
    },
    /// Branch a session at an event boundary.
    Fork {
        id: String,
        at_seq: u64,
    },
}

#[derive(Debug, Subcommand)]
enum JobCommand {
    List,
    Show {
        id: String,
    },
    Cancel {
        id: String,
    },
    /// Record a review verdict on a job awaiting review.
    Review {
        id: String,
        #[arg(value_enum)]
        verdict: Verdict,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Verdict {
    Approve,
    RequestChanges,
    Reject,
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    List,
    /// Scaffold a new plugin crate.
    New {
        name: String,
    },
    /// Show a plugin's manifest, capabilities and effective permissions.
    Show {
        id: String,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("rivet: could not start the async runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(dispatch(cli));
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}

async fn dispatch(cli: Cli) -> i32 {
    let overrides = Overrides {
        config_path: cli.config.as_ref().map(std::path::PathBuf::from),
        profile: cli.profile.clone(),
        headless: cli.headless,
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("rivet: could not read the working directory: {error}");
            return exit::CONFIG;
        }
    };

    // Every configuration problem is exit code 2, so a script can tell "I set this up
    // wrong" apart from "the agent could not do it".
    let config = match Config::load(&cwd, &overrides) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("rivet: {error}");
            return exit::CONFIG;
        }
    };

    let output = if cli.jsonl {
        Output::Jsonl
    } else {
        Output::Human
    };

    match (cli.command, cli.prompt) {
        (Some(Command::Run { prompt, job }), _) => {
            if job {
                eprintln!("rivet: `--job` needs the job runtime, which lands in Phase 5");
                return exit::CONFIG;
            }
            finish(run::start(&config, &prompt, output).await)
        }
        (None, Some(prompt)) => finish(run::start(&config, &prompt, output).await),
        (Some(Command::Resume { session }), _) => {
            match run::resume(&config, &session, output).await {
                Ok(Some(summary)) => exit::for_stop(&summary.stop),
                // Nothing to resume; the reason has already been printed.
                Ok(None) => exit::CONFIG,
                Err(error) => report(&error),
            }
        }
        (Some(Command::Session(command)), _) => {
            let result = match command {
                SessionCommand::List => session_cmd::list(&config).await,
                SessionCommand::Show { id, json } => session_cmd::show(&config, &id, json).await,
                SessionCommand::Fork { id, at_seq } => {
                    session_cmd::fork(&config, &id, at_seq).await
                }
            };
            match result {
                Ok(()) => exit::OK,
                Err(error) => report(&error),
            }
        }
        (Some(Command::Doctor), _) => match doctor::run(&config).await {
            Ok(true) => exit::OK,
            Ok(false) => exit::CONFIG,
            Err(error) => report(&error),
        },
        (Some(Command::Plugin(PluginCommand::List)), _) => match bootstrap::load(&config).await {
            Ok(loaded) => {
                for entry in &loaded.registered {
                    println!("{entry}");
                }
                for id in &loaded.deferred {
                    println!("{id} (later phase)");
                }
                loaded.shutdown();
                exit::OK
            }
            Err(error) => report(&error),
        },
        (Some(Command::Plugin(_)), _) => {
            eprintln!("rivet: plugin scaffolding and inspection land in Phase 2");
            exit::CONFIG
        }
        (Some(Command::Job(_)), _) => {
            eprintln!("rivet: the job runtime lands in Phase 5");
            exit::CONFIG
        }
        (None, None) => {
            eprintln!("rivet: nothing to do. Try `rivet \"explain this repo\"` or `rivet --help`.");
            exit::CONFIG
        }
    }
}

fn finish(result: rivet_core::Result<rivet_core::agent::RunSummary>) -> i32 {
    match result {
        Ok(summary) => exit::for_stop(&summary.stop),
        Err(error) => report(&error),
    }
}

/// Print an error and pick its exit code.
fn report(error: &rivet_core::Error) -> i32 {
    eprintln!("rivet: {error}");
    match error.kind() {
        // Setup problems, told apart from run failures so a script can react.
        rivet_core::error::ErrorKind::InvalidArgument | rivet_core::error::ErrorKind::NotFound => {
            exit::CONFIG
        }
        rivet_core::error::ErrorKind::Cancelled => exit::CANCELLED,
        rivet_core::error::ErrorKind::PolicyDenied
        | rivet_core::error::ErrorKind::ApprovalDenied => exit::POLICY,
        _ => exit::FAILED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_bare_prompt_is_accepted() {
        let cli = Cli::parse_from(["rivet", "fix the failing test"]);
        assert_eq!(cli.prompt.as_deref(), Some("fix the failing test"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn headless_and_jsonl_are_mutually_exclusive() {
        assert!(
            Cli::try_parse_from(["rivet", "--headless", "--jsonl", "x"]).is_err(),
            "two output modes at once is a user error worth catching"
        );
    }

    #[test]
    fn subcommands_parse() {
        let cli = Cli::parse_from(["rivet", "job", "review", "job_1", "approve"]);
        assert!(matches!(
            cli.command,
            Some(Command::Job(JobCommand::Review {
                verdict: Verdict::Approve,
                ..
            }))
        ));
    }

    #[test]
    fn session_show_takes_a_json_flag() {
        // `--jsonl` observes the bus; this reads the durable log. They are different
        // things and the surface should not blur them.
        let cli = Cli::parse_from(["rivet", "session", "show", "ses_1", "--json"]);
        assert!(matches!(
            cli.command,
            Some(Command::Session(SessionCommand::Show { json: true, .. }))
        ));
    }
}
