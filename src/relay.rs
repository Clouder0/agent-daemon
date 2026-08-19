//! Relay: the async-nats half of the daemon (whitepaper §5, §15.1, §10.5).
//!
//! Owns the connection, per-agent durable pull consumers, slot-driven pull
//! loops, the `Acker` implementation (ack = double ack, term), and the
//! in-progress keepalive (§5.4/ADR-0001). Pure transport: all dispatch
//! semantics live in the dispatcher; the stream itself is created by
//! `agentdctl init` (#6) via [`ensure_stream`], never by the running daemon.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::jetstream::consumer::PullConsumer;
use async_nats::jetstream::{self, Context};
use futures_util::StreamExt;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::{JoinHandle, JoinSet};

use crate::agent_id::AgentId;
use crate::config::DaemonConfig;
use crate::dispatcher::{Acker, Delivery, Dispatcher};
use crate::error::AgentdError;
use crate::logging::events;
use crate::registry::{Change, Registry};

/// Client-side cap on any single fetch request or message wait (see
/// `pull_loop`): bounds post-reconnect recovery after a dead connection.
const FETCH_CAP: Duration = Duration::from_secs(10);

/// Server-side long-poll expiry (§5.3), aligned with the client cap so both
/// ends agree on the poll lifetime and idle agents do not re-issue pulls
/// that the server would happily keep holding.
const FETCH_EXPIRES: Duration = FETCH_CAP;

/// Idle sleep when no dispatch slots are free: a freed slot is noticed
/// within ~1s without coupling the relay to the dispatcher.
const NO_SLOT_SLEEP: Duration = Duration::from_secs(1);

/// Durable consumer name: `agent-<agent_id>` verbatim (ADR-0006 — `_` is
/// legal in consumer names, so no hashing).
pub fn consumer_name(id: &AgentId) -> String {
    format!("agent-{id}")
}

/// Consumer configuration per whitepaper §5.3 + ADR-0001.
pub fn consumer_config(
    id: &AgentId,
    ack_wait: Duration,
    max_ack_pending: usize,
) -> jetstream::consumer::pull::Config {
    jetstream::consumer::pull::Config {
        durable_name: Some(consumer_name(id)),
        filter_subject: id.subject(),
        ack_policy: jetstream::consumer::AckPolicy::Explicit,
        deliver_policy: jetstream::consumer::DeliverPolicy::All,
        max_ack_pending: max_ack_pending as i64,
        ack_wait,
        ..Default::default()
    }
}

/// Operator-time stream creation for `agentdctl init` (#6): create or
/// reconcile the stream with the whitepaper §5.1 defaults. The running
/// daemon never calls this — its credentials stay consumer-only.
pub async fn ensure_stream(js: &Context, config: &DaemonConfig) -> Result<(), AgentdError> {
    let stream = jetstream::stream::Config {
        name: config.stream_name.clone(),
        subjects: vec!["agent.events.>".to_string()],
        retention: jetstream::stream::RetentionPolicy::Limits,
        storage: jetstream::stream::StorageType::File,
        max_age: Duration::from_secs(7 * 24 * 3600),
        max_message_size: config.max_event_bytes as i32,
        num_replicas: 1,
        ..Default::default()
    };
    js.get_or_create_stream(stream).await.map_err(|e| {
        AgentdError::relay(format!("cannot create stream {}: {e}", config.stream_name))
    })?;
    Ok(())
}

/// Build a JetStream context from a connected client.
pub fn jetstream_context(client: &async_nats::Client) -> Context {
    async_nats::jetstream::new(client.clone())
}

/// Connect to the relay with credentials when configured (§3.4). Initial
/// connect retries so the daemon survives a relay that is not up yet
/// (§15.1). Connection lifecycle events (disconnect/reconnect) are logged
/// per §16.
pub async fn connect(
    config: &DaemonConfig,
    nats_connected: Arc<std::sync::atomic::AtomicBool>,
) -> Result<async_nats::Client, AgentdError> {
    let options = match &config.nats_creds {
        Some(creds) => async_nats::ConnectOptions::with_credentials_file(creds.clone())
            .await
            .map_err(|e| {
                AgentdError::config(format!("cannot read credentials {}: {e}", creds.display()))
            })?,
        None => async_nats::ConnectOptions::new(),
    };
    if let Some(creds) = &config.nats_creds {
        warn_if_loose_creds(creds);
    }
    tracing::info!(
        nats_url = %crate::logging::redact_url(&config.nats_url),
        "connecting to relay (retrying until reachable)"
    );
    let event_url = config.nats_url.clone();
    let options = options.event_callback(move |event| {
        let url = event_url.clone();
        let connected = nats_connected.clone();
        async move {
            match event {
                // Reconnects surface as a fresh `Connected`.
                async_nats::Event::Connected => {
                    connected.store(true, std::sync::atomic::Ordering::Relaxed);
                    events::nats_connected(&url)
                }
                async_nats::Event::Disconnected => {
                    connected.store(false, std::sync::atomic::Ordering::Relaxed);
                    events::nats_disconnected()
                }
                async_nats::Event::LameDuckMode => {
                    tracing::warn!("relay entered lame duck mode")
                }
                other => tracing::debug!("nats connection event: {other:?}"),
            }
        }
    });
    let client = options
        .retry_on_initial_connect()
        .connect(&config.nats_url)
        .await
        .map_err(|e| AgentdError::relay(format!("cannot connect to {}: {e}", config.nats_url)))?;
    Ok(client)
}

/// `Acker` over one JetStream message: ack = double ack (§10.5), term =
/// terminal ack (§8.6/§15.2).
pub struct NatsAcker {
    message: jetstream::Message,
}

impl Acker for NatsAcker {
    async fn ack(&self) -> Result<(), String> {
        self.message
            .double_ack()
            .await
            .map_err(|e| format!("double ack failed: {e}"))
    }

    async fn term(&self) -> Result<(), String> {
        self.message
            .ack_with(jetstream::AckKind::Term)
            .await
            .map_err(|e| format!("term failed: {e}"))
    }
}

/// The relay: per-agent pull loops feeding the dispatcher.
pub struct Relay {
    js: Context,
    stream_name: String,
    registry: Arc<Registry>,
    dispatcher: Arc<Dispatcher>,
    ack_wait: Duration,
    ack_progress: Duration,
    pulls: Mutex<HashMap<AgentId, JoinHandle<()>>>,
    dispatch_tasks: Arc<AsyncMutex<JoinSet<()>>>,
}

impl Relay {
    pub fn new(
        js: Context,
        stream_name: String,
        registry: Arc<Registry>,
        dispatcher: Arc<Dispatcher>,
        ack_wait: Duration,
        ack_progress: Duration,
    ) -> Self {
        Self {
            js,
            stream_name,
            registry,
            dispatcher,
            ack_wait,
            ack_progress,
            pulls: Mutex::new(HashMap::new()),
            dispatch_tasks: Arc::new(AsyncMutex::new(JoinSet::new())),
        }
    }

    /// Bind consumers and start pull loops for every enabled agent in the
    /// registry (startup path).
    pub async fn sync_consumers(&self) -> Result<(), AgentdError> {
        let changes: Vec<Change> = self
            .registry
            .snapshot()
            .into_iter()
            .filter(|c| c.enabled)
            .map(Change::Added)
            .collect();
        self.apply_changes(&changes).await
    }

    /// Apply registry changes: bind + pull for added/enabled agents, stop
    /// pulling for disabled/removed ones (in-flight dispatches drain
    /// naturally; handlers are never killed — §7.4).
    pub async fn apply_changes(&self, changes: &[Change]) -> Result<(), AgentdError> {
        for change in changes {
            let id = change.agent_id();
            match change {
                Change::Added(c) | Change::Enabled(c) => {
                    match self.bind_consumer(&id, c.max_concurrency).await {
                        Ok(consumer) => {
                            self.start_pull(id.clone(), consumer);
                            events::consumer_bound(&id, &consumer_name(&id));
                        }
                        Err(e) => {
                            tracing::error!(agent_id = %id, "consumer bind failed; agent not consumed until next reload: {e}");
                            continue;
                        }
                    }
                    if matches!(change, Change::Added(_)) {
                        events::agent_registered(&id);
                    }
                }
                Change::Updated(_) => {
                    // Rebind in case concurrency (MaxAckPending) changed.
                    self.stop_pull(&id);
                    match self.registry.get(&id) {
                        Some(c) if c.enabled => {
                            match self.bind_consumer(&id, c.max_concurrency).await {
                                Ok(consumer) => self.start_pull(id.clone(), consumer),
                                Err(e) => {
                                    tracing::error!(agent_id = %id, "consumer rebind failed; agent not consumed until next reload: {e}")
                                }
                            }
                        }
                        _ => {}
                    }
                    events::agent_updated(&id);
                }
                Change::Disabled(_) => {
                    self.stop_pull(&id);
                    events::agent_updated(&id);
                }
                Change::Removed(_) => {
                    self.stop_pull(&id);
                    events::agent_unregistered(&id);
                }
            }
        }
        Ok(())
    }

    async fn bind_consumer(
        &self,
        id: &AgentId,
        max_concurrency: u32,
    ) -> Result<PullConsumer, AgentdError> {
        let stream = self.js.get_stream(&self.stream_name).await.map_err(|e| {
            AgentdError::relay(format!("stream {} unavailable: {e}", self.stream_name))
        })?;
        let consumer: PullConsumer = stream
            .get_or_create_consumer(
                &consumer_name(id),
                consumer_config(id, self.ack_wait, max_concurrency as usize),
            )
            .await
            .map_err(|e| AgentdError::relay(format!("cannot bind consumer for {id}: {e}")))?;
        Ok(consumer)
    }

    fn start_pull(&self, id: AgentId, consumer: PullConsumer) {
        let dispatcher = self.dispatcher.clone();
        let progress = self.ack_progress;
        let dispatch_tasks = self.dispatch_tasks.clone();
        let pull_agent = id.clone();
        let handle = tokio::spawn(async move {
            pull_loop(consumer, pull_agent, dispatcher, progress, dispatch_tasks).await;
        });
        let mut pulls = self.pulls.lock().expect("relay pulls poisoned");
        if let Some(old) = pulls.insert(id, handle) {
            old.abort();
        }
    }

    /// Best-effort consumer backlog: (num_pending, num_ack_pending).
    /// None when the stream/consumer query fails (relay down, agent not
    /// yet bound) — status stays available.
    pub async fn consumer_backlog(&self, id: &AgentId) -> Option<(u64, u64)> {
        let stream = self.js.get_stream(&self.stream_name).await.ok()?;
        let info = stream.consumer_info(&consumer_name(id)).await.ok()?;
        Some((info.num_pending as u64, info.num_ack_pending as u64))
    }

    fn stop_pull(&self, id: &AgentId) {
        if let Some(handle) = self.pulls.lock().expect("relay pulls poisoned").remove(id) {
            handle.abort();
        }
    }

    /// Graceful shutdown (§14.3): stop pulling, wait for all in-flight
    /// dispatches (which include their acks), then return. Never kills
    /// handlers; systemd's TimeoutStopSec is the backstop.
    pub async fn shutdown(&self) {
        let handles: Vec<JoinHandle<()>> = {
            let mut pulls = self.pulls.lock().expect("relay pulls poisoned");
            pulls.drain().map(|(_, h)| h).collect()
        };
        for h in handles {
            h.abort();
        }
        let mut tasks = self.dispatch_tasks.lock().await;
        while tasks.join_next().await.is_some() {}
    }
}

/// Control-plane view of the relay: apply registry changes and read consumer
/// backlog (`DaemonHandle` consumes this; tests fake it).
impl crate::control::RelayBackend for Relay {
    fn apply_changes(
        &self,
        changes: &[Change],
    ) -> impl Future<Output = Result<(), AgentdError>> + Send {
        Relay::apply_changes(self, changes)
    }

    fn consumer_backlog(&self, id: &AgentId) -> impl Future<Output = Option<(u64, u64)>> + Send {
        Relay::consumer_backlog(self, id)
    }
}

/// Slot-driven pull loop (§8.1 step 4): fetch at most the dispatcher's free
/// slots; sleep briefly when none are free.
///
/// Every fetch request and message wait is capped client-side by
/// `FETCH_CAP`: a request whose connection died mid-flight would otherwise
/// stall this loop until the *server-side* expiry (up to 30s of no pulling
/// after a relay reconnect — observed in the review smoke). With the cap,
/// recovery from any outage is bounded to ~`FETCH_CAP`.
async fn pull_loop(
    consumer: PullConsumer,
    agent: AgentId,
    dispatcher: Arc<Dispatcher>,
    ack_progress: Duration,
    dispatch_tasks: Arc<AsyncMutex<JoinSet<()>>>,
) {
    loop {
        let free = dispatcher.available(&agent);
        if free == 0 {
            tokio::time::sleep(NO_SLOT_SLEEP).await;
            continue;
        }
        let mut messages = {
            let fetch = consumer
                .batch()
                .max_messages(free.max(1))
                .expires(FETCH_EXPIRES)
                .messages();
            match tokio::time::timeout(FETCH_CAP, fetch).await {
                Ok(Ok(messages)) => messages,
                Ok(Err(e)) => {
                    tracing::warn!(agent_id = %agent, "fetch failed: {e}");
                    tokio::time::sleep(NO_SLOT_SLEEP).await;
                    continue;
                }
                Err(_) => {
                    tracing::debug!(agent_id = %agent, "fetch request stalled; re-issuing");
                    continue;
                }
            }
        };
        loop {
            let item = match tokio::time::timeout(FETCH_CAP, messages.next()).await {
                Ok(Some(item)) => item,
                Ok(None) => break, // stream ended (expiry) → re-fetch
                Err(_) => {
                    tracing::debug!(agent_id = %agent, "pull stream stalled; re-issuing");
                    break;
                }
            };
            let message = match item {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(agent_id = %agent, "message fetch error: {e}");
                    break;
                }
            };
            let info = match message.info() {
                Ok(info) => info,
                Err(e) => {
                    tracing::warn!(agent_id = %agent, "message without JetStream metadata: {e}");
                    continue;
                }
            };
            let keepalive_message = message.clone();
            let delivery = Delivery {
                agent: agent.clone(),
                raw: message.message.payload.to_vec(),
                stream_sequence: info.stream_sequence,
                consumer_sequence: info.consumer_sequence,
                delivery_count: info.delivered as u64,
                acker: NatsAcker { message },
            };
            let dispatcher = dispatcher.clone();
            let mut tasks = dispatch_tasks.lock().await;
            tasks.spawn(async move {
                // §5.4: reset AckWait while the handler runs (ADR-0001 cadence).
                let keepalive =
                    tokio::spawn(in_progress_keepalive(keepalive_message, ack_progress));
                dispatcher.dispatch(delivery).await;
                keepalive.abort();
            });
        }
    }
}

/// Send an in-progress ack every `interval` until aborted (§5.4).
async fn in_progress_keepalive(message: jetstream::Message, interval: Duration) {
    loop {
        tokio::time::sleep(interval).await;
        if let Err(e) = message.ack_with(jetstream::AckKind::Progress).await {
            tracing::debug!("in-progress ack failed: {e}");
        }
    }
}

/// §3.4 requires credentials at mode 0600; warn (do not refuse) when the
/// file is group/world accessible — the requirement is a *should*, and a
/// loud warning beats silent acceptance.
fn warn_if_loose_creds(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path)
            && meta.permissions().mode() & 0o077 != 0
        {
            tracing::warn!(
                creds = %path.display(),
                "credentials file is group/world accessible; whitepaper §3.4 requires 0600"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumer_name_is_identity_with_prefix() {
        let id = AgentId::parse("coding_main").unwrap();
        assert_eq!(consumer_name(&id), "agent-coding_main");
    }

    #[test]
    fn consumer_config_matches_whitepaper() {
        let id = AgentId::parse("coding_main").unwrap();
        let cfg = consumer_config(&id, Duration::from_secs(300), 4);
        assert_eq!(cfg.durable_name.as_deref(), Some("agent-coding_main"));
        assert_eq!(cfg.filter_subject, "agent.events.coding_main");
        assert!(matches!(
            cfg.ack_policy,
            jetstream::consumer::AckPolicy::Explicit
        ));
        assert!(matches!(
            cfg.deliver_policy,
            jetstream::consumer::DeliverPolicy::All
        ));
        assert_eq!(cfg.max_ack_pending, 4);
        assert_eq!(cfg.ack_wait, Duration::from_secs(300));
    }
}
