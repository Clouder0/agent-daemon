//! `agentd` — edge-side event dispatch daemon for Agent Native Domains.
//!
//! Turns one JetStream event for a locally registered agent into exactly one
//! local executable invocation. Mechanism only; see docs/whitepaper-v0.md.

use std::process::ExitCode;

use clap::Parser;

/// Daemon binary. Runtime behavior lands with the relay/dispatcher issues;
/// the scaffold wires the CLI shape only.
#[derive(Parser)]
#[command(
    name = "agentd",
    version,
    about = "Edge-side event dispatch daemon for Agent Native Domains",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Run the daemon in the foreground.
    Run,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run => {
            eprintln!("agentd: daemon core not yet implemented (scaffold)");
            ExitCode::FAILURE
        }
    }
}
