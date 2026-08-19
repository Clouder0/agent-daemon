//! `agentd` — edge-side event dispatch daemon for Agent Native Domains.
//!
//! Turns one JetStream event for a locally registered agent into exactly one
//! local executable invocation. Mechanism only; see docs/whitepaper-v0.md.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use clap::Parser;

use agent_daemon::config::DaemonConfig;
use agent_daemon::control::{self, DaemonHandle};
use agent_daemon::dedup::DedupStore;
use agent_daemon::dispatcher::Dispatcher;
use agent_daemon::registry::Registry;
use agent_daemon::relay::{self, Relay};

#[derive(Parser)]
#[command(
    name = "agentd",
    version,
    about = "Edge-side event dispatch daemon for Agent Native Domains",
    arg_required_else_help = true
)]
struct Cli {
    /// Path to the daemon config file
    /// (default: $XDG_CONFIG_HOME/agentd/agentd.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Run the daemon in the foreground.
    Run,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run => run(cli.config).await,
    }
}

async fn run(config_path: Option<PathBuf>) -> ExitCode {
    // Config: explicit path must exist; the default path may be absent
    // (defaults apply, noted loudly).
    let config = match load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("agentd: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = agent_daemon::logging::init(&config) {
        eprintln!("agentd: {e}");
        return ExitCode::FAILURE;
    }

    // Dedup store: a corrupt/unopenable store is a startup error (the
    // operator decides) — never silently discarded history.
    let dedup = match DedupStore::open(&config.resolved_dedup_path(), config.dedup_ttl()) {
        Ok(d) => Arc::new(d),
        Err(e) => {
            tracing::error!("cannot open dedup store: {e}");
            return ExitCode::FAILURE;
        }
    };

    let registry = match Registry::load(&config.resolved_agents_dir()) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            tracing::error!("cannot load agent registry: {e}");
            return ExitCode::FAILURE;
        }
    };

    let dispatcher = Arc::new(Dispatcher::new(
        registry.clone(),
        dedup,
        Duration::from_secs(config.slow_handler_warn_secs),
        config.max_event_bytes as usize,
    ));

    // Connect (retries until the relay is reachable — §15.1) and bind
    // per-agent consumers. The connected flag feeds `agentdctl status`.
    let nats_connected = Arc::new(AtomicBool::new(false));
    let client = match relay::connect(&config, nats_connected.clone()).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("relay connection failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let js = relay::jetstream_context(&client);
    let relay = Arc::new(Relay::new(
        js,
        config.stream_name.clone(),
        registry.clone(),
        dispatcher.clone(),
        Duration::from_secs(config.ack_wait_secs),
        Duration::from_secs(config.ack_progress_interval_secs),
    ));
    if let Err(e) = relay.sync_consumers().await {
        tracing::error!("consumer sync failed: {e}");
    }

    // Control socket (§7.3/§18): SIGHUP and agentdctl share one apply path.
    let handle = Arc::new(DaemonHandle::new(
        registry.clone(),
        dispatcher.clone(),
        relay.clone(),
        nats_connected,
    ));
    let socket_path = control::socket_path(&config);
    let listener = match control::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("cannot bind control socket: {e}");
            return ExitCode::FAILURE;
        }
    };
    tokio::spawn(control::serve(handle.clone(), listener));

    // SIGHUP → reload agents.d and apply the diff; SIGTERM/Ctrl-C → graceful
    // shutdown (§14.3): stop pulling, let in-flight handlers finish and ack.
    let mut sighup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("cannot install SIGHUP handler: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("cannot install SIGTERM handler: {e}");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(
        agents = registry.snapshot().len(),
        control_socket = %socket_path.display(),
        "agentd running"
    );
    loop {
        tokio::select! {
            _ = sighup.recv() => {
                if let Err(e) = handle.reload().await {
                    tracing::error!("reload failed; keeping previous registry: {e}");
                }
            }
            _ = sigterm.recv() => {
                tracing::info!("SIGTERM: draining in-flight handlers (§14.3)");
                relay.shutdown().await;
                let _ = std::fs::remove_file(&socket_path);
                tracing::info!("agentd stopped");
                return ExitCode::SUCCESS;
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl-C: draining in-flight handlers (§14.3)");
                relay.shutdown().await;
                let _ = std::fs::remove_file(&socket_path);
                tracing::info!("agentd stopped");
                return ExitCode::SUCCESS;
            }
        }
    }
}

fn load_config(config_path: Option<PathBuf>) -> Result<DaemonConfig, String> {
    match config_path {
        Some(path) => DaemonConfig::load(&path).map_err(|e| e.to_string()),
        None => {
            let path = default_config_path();
            if path.exists() {
                DaemonConfig::load(&path).map_err(|e| e.to_string())
            } else {
                eprintln!("agentd: no config at {}; using defaults", path.display());
                Ok(DaemonConfig::default())
            }
        }
    }
}

fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_default()
        .join("agentd")
        .join("agentd.toml")
}
