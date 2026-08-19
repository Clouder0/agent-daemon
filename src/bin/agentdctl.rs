//! `agentdctl` — control CLI for a local agentd (whitepaper §7.3, §18).
//!
//! `init` talks to the relay directly (operator-time stream creation with
//! operator credentials — v0.1); everything else talks to the daemon's
//! control socket.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use agent_daemon::agent_id::AgentId;
use agent_daemon::config::DaemonConfig;
use agent_daemon::control::{Request, Response, StatusReport};
use agent_daemon::registry::AgentConfig;
use agent_daemon::relay;

#[derive(Parser)]
#[command(
    name = "agentdctl",
    version,
    about = "Control CLI for the agentd daemon",
    arg_required_else_help = true
)]
struct Cli {
    /// Path to the daemon config file (for socket/relay defaults).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Control socket path (default: resolved from the config).
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create or reconcile the JetStream stream on the relay
    /// (operator-time, one-shot; uses operator credentials — v0.1).
    Init {
        /// Operator credentials file (default: the config's `nats_creds`).
        #[arg(long)]
        creds: Option<PathBuf>,
        /// Relay URL (default: the config's `nats_url`).
        #[arg(long)]
        url: Option<String>,
    },

    /// Register a local agent.
    Register {
        /// Agent id, e.g. `coding_main`.
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

    /// Update a registered agent (flags merge over the current config).
    Update {
        /// Agent id, e.g. `coding_main`.
        id: String,

        /// New handler executable path.
        #[arg(long)]
        handler: Option<PathBuf>,

        /// New maximum concurrency.
        #[arg(long)]
        max_concurrency: Option<u32>,

        /// New working directory.
        #[arg(long)]
        cwd: Option<PathBuf>,

        /// Enable the agent.
        #[arg(long, conflicts_with = "disable")]
        enable: bool,

        /// Disable the agent (stops consuming; in-flight drains).
        #[arg(long)]
        disable: bool,
    },

    /// Unregister a local agent.
    Unregister {
        /// Agent id, e.g. `coding_main`.
        id: String,
    },

    /// List registered agents.
    List,

    /// Reload agent registrations from disk.
    Reload,

    /// Show daemon status.
    Status,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match &cli.command {
        Command::Init { creds, url } => init(cli.config.clone(), creds.clone(), url.clone()).await,
        Command::Register {
            id,
            handler,
            max_concurrency,
            cwd,
        } => {
            let agent = match agent_config(id, handler.clone(), *max_concurrency, cwd.clone(), true)
            {
                Ok(a) => a,
                Err(e) => return fail(e),
            };
            rpc_ok(&cli, Request::Register { agent }).await
        }
        Command::Update {
            id,
            handler,
            max_concurrency,
            cwd,
            enable,
            disable,
        } => {
            update(
                &cli,
                id,
                handler.clone(),
                *max_concurrency,
                cwd.clone(),
                *enable,
                *disable,
            )
            .await
        }
        Command::Unregister { id } => {
            let agent_id = match AgentId::parse(id) {
                Ok(a) => a,
                Err(e) => return fail(format!("invalid agent id: {e}")),
            };
            rpc_ok(&cli, Request::Unregister { agent_id }).await
        }
        Command::List => list(&cli).await,
        Command::Reload => rpc_ok(&cli, Request::Reload).await,
        Command::Status => status(&cli).await,
    }
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("agentdctl: {msg}");
    ExitCode::FAILURE
}

fn load_config_or_default(path: Option<&Path>) -> DaemonConfig {
    match path {
        Some(p) => DaemonConfig::load(p).unwrap_or_else(|e| {
            eprintln!("agentdctl: cannot load config {}: {e}", p.display());
            std::process::exit(2);
        }),
        None => {
            let p = default_config_path();
            if p.exists() {
                DaemonConfig::load(&p).unwrap_or_else(|e| {
                    eprintln!("agentdctl: cannot load config {}: {e}", p.display());
                    std::process::exit(2);
                })
            } else {
                DaemonConfig::default()
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

fn agent_config(
    id: &str,
    handler: PathBuf,
    max_concurrency: u32,
    cwd: Option<PathBuf>,
    enabled: bool,
) -> Result<AgentConfig, String> {
    let agent_id = AgentId::parse(id).map_err(|e| format!("invalid agent id: {e}"))?;
    Ok(AgentConfig {
        agent_id,
        handler,
        max_concurrency,
        working_directory: cwd,
        enabled,
    })
}

async fn init(
    config_path: Option<PathBuf>,
    creds: Option<PathBuf>,
    url: Option<String>,
) -> ExitCode {
    let mut config = load_config_or_default(config_path.as_deref());
    if let Some(creds) = creds {
        config.nats_creds = Some(creds);
    }
    if let Some(url) = url {
        config.nats_url = url;
    }
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = match relay::connect(&config, flag).await {
        Ok(c) => c,
        Err(e) => return fail(format!("cannot connect to {}: {e}", config.nats_url)),
    };
    let js = relay::jetstream_context(&client);
    if let Err(e) = relay::ensure_stream(&js, &config).await {
        return fail(format!("stream init failed: {e}"));
    }
    println!("stream {} ready ({})", config.stream_name, config.nats_url);
    ExitCode::SUCCESS
}

async fn socket(cli: &Cli) -> PathBuf {
    if let Some(s) = &cli.socket {
        return s.clone();
    }
    let config = load_config_or_default(cli.config.as_deref());
    agent_daemon::control::socket_path(&config)
}

async fn rpc(socket: &Path, request: &Request) -> Result<Response, String> {
    let stream = UnixStream::connect(socket).await.map_err(|e| {
        format!(
            "is agentd running? cannot connect to control socket {}: {e}",
            socket.display()
        )
    })?;
    let (reader, mut writer) = stream.into_split();
    let mut line = serde_json::to_string(request).map_err(|e| e.to_string())?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("write failed: {e}"))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("flush failed: {e}"))?;
    let mut lines = BufReader::new(reader).lines();
    let response = lines
        .next_line()
        .await
        .map_err(|e| format!("read failed: {e}"))?
        .ok_or("daemon closed the connection")?;
    serde_json::from_str(&response).map_err(|e| format!("malformed response: {e}"))
}

async fn rpc_ok(cli: &Cli, request: Request) -> ExitCode {
    let socket_path = socket(cli).await;
    match rpc(&socket_path, &request).await {
        Ok(r) if r.ok => {
            println!("ok");
            ExitCode::SUCCESS
        }
        Ok(r) => fail(r.error.as_deref().unwrap_or("unknown error")),
        Err(e) => fail(e),
    }
}

async fn update(
    cli: &Cli,
    id: &str,
    handler: Option<PathBuf>,
    max_concurrency: Option<u32>,
    cwd: Option<PathBuf>,
    enable: bool,
    disable: bool,
) -> ExitCode {
    let socket_path = socket(cli).await;
    // Fetch the current config, merge flags, send a full Update.
    let current = match rpc(&socket_path, &Request::List).await {
        Ok(r) if r.ok => r.agents,
        Ok(r) => return fail(r.error.as_deref().unwrap_or("list failed")),
        Err(e) => return fail(e),
    };
    let Some(mut current) = current
        .unwrap_or_default()
        .into_iter()
        .find(|a| a.agent_id.as_str() == id)
    else {
        return fail(format!("agent {id} is not registered"));
    };
    if let Some(handler) = handler {
        current.handler = handler;
    }
    if let Some(max_concurrency) = max_concurrency {
        current.max_concurrency = max_concurrency;
    }
    if let Some(cwd) = cwd {
        current.working_directory = Some(cwd);
    }
    if enable || disable {
        current.enabled = enable;
    }
    let agent_id = current.agent_id.clone();
    match rpc(&socket_path, &Request::Update { agent: current }).await {
        Ok(r) if r.ok => {
            println!("updated {agent_id}");
            ExitCode::SUCCESS
        }
        Ok(r) => fail(r.error.as_deref().unwrap_or("unknown error")),
        Err(e) => fail(e),
    }
}

async fn list(cli: &Cli) -> ExitCode {
    let socket_path = socket(cli).await;
    match rpc(&socket_path, &Request::List).await {
        Ok(r) if r.ok => {
            let agents = r.agents.unwrap_or_default();
            if agents.is_empty() {
                println!("no agents registered");
                return ExitCode::SUCCESS;
            }
            println!(
                "{:<24} {:<8} {:<5} {:<24}",
                "AGENT", "STATE", "CONC", "HANDLER"
            );
            for a in agents {
                println!(
                    "{:<24} {:<8} {:<5} {:<24}",
                    a.agent_id,
                    if a.enabled { "enabled" } else { "disabled" },
                    a.max_concurrency,
                    a.handler.display().to_string()
                );
            }
            ExitCode::SUCCESS
        }
        Ok(r) => fail(r.error.as_deref().unwrap_or("unknown error")),
        Err(e) => fail(e),
    }
}

async fn status(cli: &Cli) -> ExitCode {
    let socket_path = socket(cli).await;
    match rpc(&socket_path, &Request::Status).await {
        Ok(r) if r.ok => {
            let Some(status) = r.status else {
                return fail("daemon returned no status");
            };
            print_status(&status);
            ExitCode::SUCCESS
        }
        Ok(r) => fail(r.error.as_deref().unwrap_or("unknown error")),
        Err(e) => fail(e),
    }
}

fn print_status(status: &StatusReport) {
    println!(
        "nats: {}",
        if status.nats_connected {
            "connected"
        } else {
            "disconnected"
        }
    );
    println!(
        "{:<24} {:<8} {:<5} {:<9} {:<8} {:<8}",
        "AGENT", "STATE", "CONC", "INFLIGHT", "PENDING", "ACKPEND"
    );
    for a in &status.agents {
        println!(
            "{:<24} {:<8} {:<5} {:<9} {:<8} {:<8}",
            a.agent_id,
            if a.enabled { "enabled" } else { "disabled" },
            a.max_concurrency,
            a.in_flight,
            opt(a.num_pending),
            opt(a.num_ack_pending),
        );
    }
}

fn opt(v: Option<u64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string())
}
