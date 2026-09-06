//! The `rivet` binary.
//!
//! The CLI is a *client of the runtime*, on exactly the same footing as the TUI: it
//! subscribes to events and renders them. Nothing here reaches into the agent loop.
//!
//! Phase 0 ships the command surface only, so the shape of the UX is reviewable before
//! the runtime exists to serve it.

use clap::{Parser, Subcommand};

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
    #[arg(long, global = true, conflicts_with = "headless")]
    jsonl: bool,

    /// Override the configured policy profile.
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
    /// Run a prompt, optionally as a tracked task.
    Run {
        prompt: String,
        /// Create a task and drive it through the configured workflow.
        #[arg(long)]
        task: bool,
    },
    /// Resume a previous session.
    Resume { session: String },
    /// Inspect sessions.
    #[command(subcommand)]
    Session(SessionCommand),
    /// Inspect and drive tasks.
    #[command(subcommand)]
    Task(TaskCommand),
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
    },
    /// Branch a session at an event boundary.
    Fork {
        id: String,
        at_seq: u64,
    },
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    List,
    Show {
        id: String,
    },
    Cancel {
        id: String,
    },
    /// Record a review verdict on a task awaiting review.
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

fn main() {
    let cli = Cli::parse();
    eprintln!(
        "rivet {}: command surface only; the runtime lands in Phase 1. \
         Parsed: {cli:?}",
        env!("CARGO_PKG_VERSION")
    );
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
        let cli = Cli::parse_from(["rivet", "task", "review", "tsk_1", "approve"]);
        assert!(matches!(
            cli.command,
            Some(Command::Task(TaskCommand::Review {
                verdict: Verdict::Approve,
                ..
            }))
        ));
    }
}
