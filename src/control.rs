//! Local control plane (whitepaper §7.3, §18): a Unix control socket that
//! accepts one JSON request per line and serves register/update/unregister/
//! list/reload/status. Same-user trust only — mode 0600, no additional
//! authentication (§7.3).
//!
//! All mutations flow through [`DaemonHandle`], the single apply path also
//! used by SIGHUP: a socket `register` binds the consumer immediately — no
//! restart, no divergence between reload-time and control-time behavior.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::agent_id::AgentId;
use crate::config::DaemonConfig;
use crate::dispatcher::Dispatcher;
use crate::error::AgentdError;
use crate::registry::{AgentConfig, Change, Registry};

/// Relay-side capabilities the control plane needs. Implemented by
/// [`crate::relay::Relay`]; tests use a recording fake (the same seam
/// pattern as `Acker`/`DedupCheck`).
pub trait RelayBackend: Send + Sync {
    fn apply_changes(
        &self,
        changes: &[Change],
    ) -> impl Future<Output = Result<(), AgentdError>> + Send;

    /// Best-effort consumer backlog: `(num_pending, num_ack_pending)`.
    fn consumer_backlog(&self, id: &AgentId) -> impl Future<Output = Option<(u64, u64)>> + Send;
}

/// Everything the control plane (and SIGHUP) drives. One type, one apply
/// path.
pub struct DaemonHandle<A: RelayBackend> {
    pub registry: Arc<Registry>,
    pub dispatcher: Arc<Dispatcher>,
    pub backend: Arc<A>,
    pub nats_connected: Arc<AtomicBool>,
}

impl<A: RelayBackend + 'static> DaemonHandle<A> {
    pub fn new(
        registry: Arc<Registry>,
        dispatcher: Arc<Dispatcher>,
        backend: Arc<A>,
        nats_connected: Arc<AtomicBool>,
    ) -> Self {
        Self {
            registry,
            dispatcher,
            backend,
            nats_connected,
        }
    }

    /// The single apply path: registry diff → dispatcher state → relay
    /// consumers. Shared by SIGHUP and every control op.
    pub async fn apply(&self, changes: &[Change]) {
        self.dispatcher.apply_changes(changes);
        if let Err(e) = self.backend.apply_changes(changes).await {
            tracing::error!("applying registry changes failed: {e}");
        }
    }

    pub async fn register(&self, agent: AgentConfig) -> Result<(), AgentdError> {
        self.registry.register(&agent)?;
        self.apply(&[Change::Added(agent)]).await;
        Ok(())
    }

    pub async fn update(&self, agent: AgentConfig) -> Result<(), AgentdError> {
        self.registry.update(&agent)?;
        self.apply(&[Change::Updated(agent)]).await;
        Ok(())
    }

    pub async fn unregister(&self, id: AgentId) -> Result<(), AgentdError> {
        self.registry.unregister(&id)?;
        self.apply(&[Change::Removed(id)]).await;
        Ok(())
    }

    pub async fn reload(&self) -> Result<Vec<Change>, AgentdError> {
        let changes = self.registry.reload()?;
        self.apply(&changes).await;
        Ok(changes)
    }
}

/// One control request (§18 wire shape: `{"op":"register","agent":{…}}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    Register { agent: AgentConfig },
    Update { agent: AgentConfig },
    Unregister { agent_id: AgentId },
    List,
    Reload,
    Status,
}

/// Per-agent status view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    pub agent_id: AgentId,
    pub handler: PathBuf,
    pub max_concurrency: u32,
    pub enabled: bool,
    pub in_flight: usize,
    /// Undelivered messages waiting in the consumer (None when the query
    /// failed — best effort).
    #[serde(default)]
    pub num_pending: Option<u64>,
    /// Delivered but not yet acked.
    #[serde(default)]
    pub num_ack_pending: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    pub nats_connected: bool,
    pub agents: Vec<AgentStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<AgentConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusReport>,
}

impl Response {
    fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            agents: None,
            status: None,
        }
    }

    fn err(e: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            error: Some(e.to_string()),
            agents: None,
            status: None,
        }
    }
}

/// Bind the control socket: remove a stale file, bind, restrict to 0600.
pub fn bind(socket_path: &Path) -> Result<UnixListener, AgentdError> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path).map_err(|e| {
            AgentdError::config(format!(
                "cannot remove stale control socket {}: {e}",
                socket_path.display()
            ))
        })?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentdError::config(format!("cannot create {}: {e}", parent.display())))?;
    }
    let listener = UnixListener::bind(socket_path).map_err(|e| {
        AgentdError::config(format!(
            "cannot bind control socket {}: {e}",
            socket_path.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |e| {
                AgentdError::config(format!(
                    "cannot chmod control socket {}: {e}",
                    socket_path.display()
                ))
            },
        )?;
    }
    Ok(listener)
}

/// Serve connections until the task is aborted. Each connection handles one
/// JSON request per line; concurrent connections are fine (registry
/// mutations serialize internally).
pub async fn serve<A: RelayBackend + 'static>(
    handle: Arc<DaemonHandle<A>>,
    listener: UnixListener,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let handle = handle.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(handle, stream).await {
                        tracing::debug!("control connection error: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("control socket accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn serve_connection<A: RelayBackend + 'static>(
    handle: Arc<DaemonHandle<A>>,
    stream: UnixStream,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handle_request(&handle, request).await,
            Err(e) => Response::err(format!("malformed request: {e}")),
        };
        let mut out = serde_json::to_string(&response).unwrap_or_else(|_| {
            r#"{"ok":false,"error":"response serialization failed"}"#.to_string()
        });
        out.push('\n');
        writer.write_all(out.as_bytes()).await?;
        writer.flush().await?;
    }
    Ok(())
}

async fn handle_request<A: RelayBackend + 'static>(
    handle: &Arc<DaemonHandle<A>>,
    request: Request,
) -> Response {
    match request {
        Request::Register { agent } => handle.register(agent).await.into_resp(),
        Request::Update { agent } => handle.update(agent).await.into_resp(),
        Request::Unregister { agent_id } => handle.unregister(agent_id).await.into_resp(),
        Request::List => {
            let mut r = Response::ok();
            r.agents = Some(handle.registry.snapshot());
            r
        }
        Request::Reload => handle.reload().await.map(|_| ()).into_resp(),
        Request::Status => {
            let mut agents = Vec::new();
            for config in handle.registry.snapshot() {
                let (num_pending, num_ack_pending) = handle
                    .backend
                    .consumer_backlog(&config.agent_id)
                    .await
                    .map(|(pending, acked)| (Some(pending), Some(acked)))
                    .unwrap_or((None, None));
                agents.push(AgentStatus {
                    agent_id: config.agent_id.clone(),
                    handler: config.handler.clone(),
                    max_concurrency: config.max_concurrency,
                    enabled: config.enabled,
                    in_flight: handle.dispatcher.in_flight(&config.agent_id),
                    num_pending,
                    num_ack_pending,
                });
            }
            Response {
                ok: true,
                error: None,
                agents: None,
                status: Some(StatusReport {
                    nats_connected: handle.nats_connected.load(Ordering::Relaxed),
                    agents,
                }),
            }
        }
    }
}

trait IntoResponse<T> {
    fn into_resp(self) -> Response;
}

impl<T> IntoResponse<T> for Result<T, AgentdError> {
    fn into_resp(self) -> Response {
        match self {
            Ok(_) => Response::ok(),
            Err(e) => Response::err(e),
        }
    }
}

/// Resolve the control socket path from config (used by the daemon and
/// `agentdctl` alike).
pub fn socket_path(config: &DaemonConfig) -> PathBuf {
    config.control_socket.clone().unwrap_or_else(|| {
        dirs::runtime_dir()
            .unwrap_or_default()
            .join("agentd")
            .join("control.sock")
    })
}
