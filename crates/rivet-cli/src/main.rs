//! The `rivet` binary.
//!
//! The CLI is a *client of the runtime*, on exactly the same footing as the TUI: it
//! subscribes to events and renders them. Nothing here reaches into the agent loop.
//!
//! Phase 0 shipped the command surface; Phase 1 connects it to a runtime that exists. The
//! surface itself is unchanged, because it was reviewed as the shape of the UX.

mod catalog;
mod config;
mod doctor;
mod exit;
mod plugin_cmd;
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

    /// Draw the full-screen UI: job panel, agent panel, status bar.
    ///
    /// Opt-in in Phase 3. It needs a terminal, so it refuses a pipe rather than putting one
    /// into raw mode and leaving nothing to restore.
    #[arg(long, global = true, conflicts_with_all = ["headless", "jsonl"])]
    tui: bool,

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

/// How long a still-running blocking task may delay process exit.
///
/// Long enough for work that is genuinely finishing, short enough that an uncooperative
/// tool cannot turn Ctrl-C into a hang.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// The default `tracing` filter.
///
/// `warn` for everything, plus `info` for the telemetry plugin — a structured-log plugin
/// that is switched on and emits nothing visible is not a structured-log plugin. `RUST_LOG`
/// still overrides the whole thing.
///
/// This does not change the default run's output: `rivet.telemetry-log` is outside
/// `catalog::default_selection`, so on a tree nobody configured this directive has nothing
/// to point at.
const DEFAULT_LOG_FILTER: &str = "warn,rivet_telemetry_log=info";

/// Environment variable selecting the log format. `json` gives one JSON object per record.
const LOG_FORMAT_ENV: &str = "RIVET_LOG_FORMAT";

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    init_tracing();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("rivet: could not start the async runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(dispatch(cli));

    // Do not let a wedged blocking task hold the process open. Dropping a runtime waits
    // for every `spawn_blocking` thread, and a tool that ignored cancellation is exactly
    // such a thread -- `read_file` on a named pipe with no writer never returns. The loop
    // already abandoned it inside the five-second budget and wrote the log; waiting for
    // it here would spend that budget and then hang anyway, which is the difference
    // between "Ctrl-C stops the run" and "Ctrl-C needs a second Ctrl-C".
    //
    // The grace is for blocking work that is about to finish on its own; anything still
    // running after it is left to the exiting process.
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Set up `tracing`'s stderr sink.
///
/// The JSON layer is behind an environment variable rather than a flag because the choice
/// belongs to whoever is *collecting* the logs, not to whoever typed the prompt — and
/// because `--jsonl` already means something else on this binary. Without it "structured
/// log" would be half true: the fields exist, and nothing machine-readable comes out.
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER));
    let json = std::env::var(LOG_FORMAT_ENV).is_ok_and(|value| value == "json");
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    if json {
        builder.json().init();
    } else {
        builder.init();
    }
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
    } else if cli.tui {
        Output::Tui
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
        (Some(Command::Plugin(command)), _) => {
            // None of these load a plugin, so none of them needs a credential.
            let result = match command {
                PluginCommand::List => plugin_cmd::list(&config),
                PluginCommand::Show { id } => plugin_cmd::show(&config, &id),
                PluginCommand::New { name } => plugin_cmd::new(&name),
            };
            match result {
                Ok(()) => exit::OK,
                Err(error) => report(&error),
            }
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
    fn the_three_output_modes_are_mutually_exclusive() {
        for pair in [
            ["--tui", "--jsonl"],
            ["--tui", "--headless"],
            ["--jsonl", "--headless"],
        ] {
            assert!(
                Cli::try_parse_from(["rivet", pair[0], pair[1], "x"]).is_err(),
                "{pair:?} should not parse together"
            );
        }
        assert!(Cli::try_parse_from(["rivet", "--tui", "x"]).is_ok());
    }

    #[test]
    fn the_default_log_filter_only_raises_the_telemetry_plugin() {
        // Changing the CLI's default stderr behavior is worth being deliberate about. The
        // directive names one target and leaves everything else at `warn`.
        assert!(DEFAULT_LOG_FILTER.starts_with("warn"));
        assert_eq!(
            DEFAULT_LOG_FILTER.matches('=').count(),
            1,
            "one target is raised, and it is the telemetry plugin: {DEFAULT_LOG_FILTER}"
        );
        assert!(DEFAULT_LOG_FILTER.contains("rivet_telemetry_log=info"));
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
