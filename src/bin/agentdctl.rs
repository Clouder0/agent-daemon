//! `agentdctl` — control CLI for a local agentd (whitepaper §7.3, §18).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agentdctl",
    version,
    about = "Control CLI for the agentd daemon",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create or reconcile the JetStream stream on the relay.
    ///
    /// Operator-time, one-shot setup using operator-grade credentials
    /// (ADR/v0.1): the running daemon's credentials never need
    /// stream-creation permission.
    Init,

    /// Register a local agent.
    Register {
        /// Agent id, e.g. `coding.main`.
        #[arg(long)]
        id: String,

        /// Absolute path to the handler executable.
        #[arg(long)]
        handler: PathBuf,

        /// Maximum concurrent handler invocations (default serial).
        #[arg(long, default_value_t = 1)]
        max_concurrency: u32,

        /// Optional handler working directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },

    /// Update a registered agent.
    Update {
        /// Agent id, e.g. `coding.main`.
        id: String,

        /// New handler executable path.
        #[arg(long)]
        handler: Option<PathBuf>,
    },

    /// Unregister a local agent.
    Unregister {
        /// Agent id, e.g. `coding.main`.
        id: String,
    },

    /// List registered agents.
    List,

    /// Reload agent registrations from disk.
    Reload,

    /// Show daemon status.
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    eprintln!(
        "agentdctl: {:?} not yet implemented (scaffold)",
        cli.command
    );
    ExitCode::FAILURE
}
