//! Event dispatcher (whitepaper §8, ADR-0001/0005): turn one relay delivery
//! into exactly one local handler invocation. Mechanism only — no retries on
//! any exit code, no timeouts; every failure is terminal and logged.
//!
//! Testability seams (the only two traits): [`Acker`] is the relay boundary
//! (#2 implements it with async-nats; tests record acks/terms), and
//! [`DedupCheck`] wraps the dedup store so the fail-open policy is testable.
//!
//! Fail-open policy (ADR-0005): a broken dedup store never blocks dispatch
//! and never amplifies duplicates — `is_completed` errors dispatch anyway;
//! `mark_completed` errors after the handler ran still ack. Both log at
//! ERROR.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::agent_id::AgentId;
use crate::dedup::DedupStore;
use crate::error::AgentdError;
use crate::event::EventEnvelope;
use crate::logging::{self, events};
use crate::registry::{Change, Registry};

/// Ack handle for one delivery. The relay (#2) implements this with
/// async-nats (`ack` = double ack, whitepaper §10.5).
pub trait Acker: Send + Sync {
    fn ack(&self) -> impl Future<Output = Result<(), String>> + Send;
    fn term(&self) -> impl Future<Output = Result<(), String>> + Send;
}

/// The dedup-store seam: two methods, so the dispatcher's fail-open policy
/// can be tested with a failing fake.
pub trait DedupCheck: Send + Sync {
    fn is_completed(&self, agent_id: &AgentId, event_id: &str) -> Result<bool, AgentdError>;
    fn mark_completed(&self, agent_id: &AgentId, event_id: &str) -> Result<(), AgentdError>;
}

impl DedupCheck for DedupStore {
    fn is_completed(&self, agent_id: &AgentId, event_id: &str) -> Result<bool, AgentdError> {
        DedupStore::is_completed(self, agent_id, event_id)
    }

    fn mark_completed(&self, agent_id: &AgentId, event_id: &str) -> Result<(), AgentdError> {
        DedupStore::mark_completed(self, agent_id, event_id)
    }
}

/// One message handed from the relay to the dispatcher. `agent` is the
/// consumer's agent — the envelope's `agent_id` must match it (§15.2).
pub struct Delivery<A: Acker> {
    pub agent: AgentId,
    /// The original message bytes; delivered to the handler's stdin
    /// unmodified (§8.2 — never re-serialized).
    pub raw: Vec<u8>,
    pub stream_sequence: u64,
    pub consumer_sequence: u64,
    pub delivery_count: u64,
    pub acker: A,
}

struct AgentState {
    permits: Arc<Semaphore>,
    /// In-flight event ids for this agent; check-and-insert is one critical
    /// section (ADR-0001 + ADR-0005 keying: redelivery is per-agent).
    in_flight: HashSet<String>,
}

/// The dispatcher. Dispatch tasks acquire a concurrency permit per agent
/// (`max_concurrency`, default 1 = serial, §9.1), run the §8.1 sequence, and
/// always resolve the delivery (ack or term) — never retry.
pub struct Dispatcher {
    registry: Arc<Registry>,
    dedup: Arc<dyn DedupCheck>,
    slow_handler_warn: Duration,
    max_event_bytes: usize,
    states: Mutex<HashMap<AgentId, AgentState>>,
}

impl Dispatcher {
    pub fn new(
        registry: Arc<Registry>,
        dedup: Arc<dyn DedupCheck>,
        slow_handler_warn: Duration,
        max_event_bytes: usize,
    ) -> Self {
        Self {
            registry,
            dedup,
            slow_handler_warn,
            max_event_bytes,
            states: Mutex::new(HashMap::new()),
        }
    }

    /// Free dispatch slots for an agent — the relay (#2) pulls at most this
    /// many messages for the agent's consumer (§8.1 step 4). Zero when the
    /// agent is unregistered or disabled.
    pub fn available(&self, agent: &AgentId) -> usize {
        match self.registry.get(agent) {
            Some(config) if config.enabled => {
                let permits = self.ensure_state(agent, config.max_concurrency);
                permits.available_permits()
            }
            _ => 0,
        }
    }

    /// Apply registry reload changes. State is recreated lazily with the new
    /// concurrency; Removed/Disabled stop new dispatches via the registry
    /// checks. In-flight handlers drain naturally on the old semaphore and
    /// are never killed (§7.4).
    pub fn apply_changes(&self, changes: &[Change]) {
        let mut states = self.states.lock().expect("dispatcher states poisoned");
        for change in changes {
            states.remove(&change.agent_id());
        }
    }

    /// Dispatch one delivery through the §8.1 sequence. Always resolves the
    /// delivery (ack or term) except the in-flight-drop path, which drops
    /// the local copy *without* acking so JetStream redelivers after the
    /// first dispatch completes (ADR-0001).
    pub async fn dispatch<A: Acker>(&self, delivery: Delivery<A>) {
        let Delivery {
            agent,
            raw,
            stream_sequence,
            consumer_sequence,
            delivery_count,
            acker,
        } = delivery;

        // Size gate (§12.2/§15.2).
        if raw.len() > self.max_event_bytes {
            events::invalid_event(&format!(
                "event exceeds size limit ({} > {})",
                raw.len(),
                self.max_event_bytes
            ));
            let _ = acker.term().await;
            return;
        }

        // Parse + validate (§15.2 terminal on failure).
        let envelope = match EventEnvelope::parse(&raw) {
            Ok(e) => e,
            Err(e) => {
                events::invalid_event(&e.to_string());
                let _ = acker.term().await;
                return;
            }
        };

        // Registration check (step 3): the envelope's agent_id must match
        // the consumer's agent, and the agent must be registered + enabled.
        if envelope.agent_id != agent {
            events::invalid_event(&format!(
                "envelope agent_id {} does not match consumer agent {agent}",
                envelope.agent_id
            ));
            let _ = acker.term().await;
            return;
        }
        let Some(config) = self.registry.get(&agent) else {
            tracing::warn!(agent_id = %agent, "delivery for unregistered agent");
            let _ = acker.term().await;
            return;
        };
        if !config.enabled {
            tracing::warn!(agent_id = %agent, "delivery for disabled agent");
            let _ = acker.term().await;
            return;
        }

        // §16 correlation: the dispatch span is entered around synchronous
        // emissions (guards cannot be held across await points).
        let dispatch_span = logging::dispatch_span(
            &agent,
            &envelope.event_id,
            &format!("agent-{}", agent.as_str()), // refined by #2's consumer naming
            stream_sequence,
        );
        {
            let _g = dispatch_span.enter();
            events::event_received();
        }

        // Concurrency slot (step 4).
        let permits = self.ensure_state(&agent, config.max_concurrency);
        let state_permits = permits.clone();
        let _permit = permits.acquire_owned().await.expect("semaphore closed");

        // Completed dedup (step 5) — fail-open on store error.
        match self.dedup.is_completed(&agent, &envelope.event_id) {
            Ok(true) => {
                {
                    let _g = dispatch_span.enter();
                    events::dedup_hit();
                }
                if let Err(e) = acker.ack().await {
                    events::ack_failure(&e);
                } else {
                    events::ack_success();
                }
                return;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::error!(agent_id = %agent, "dedup lookup failed; dispatching anyway: {e}");
            }
        }

        // In-flight check-and-insert (step 6, one critical section).
        {
            let mut states = self.states.lock().expect("dispatcher states poisoned");
            let entry = states.entry(agent.clone()).or_insert_with(|| AgentState {
                permits: state_permits.clone(),
                in_flight: HashSet::new(),
            });
            if entry.in_flight.contains(&envelope.event_id) {
                let _g = dispatch_span.enter();
                events::in_flight_duplicate_dropped();
                return; // no ack — JetStream redelivers after AckWait
            }
            entry.in_flight.insert(envelope.event_id.clone());
        }

        // Spawn (step 7). Absolute path from local config only; never a
        // shell (§7.2). §8.3 env vars; working directory honored.
        let handler_path = config.handler.display().to_string();
        let mut command = Command::new(&config.handler);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .env("AGENTD_AGENT_ID", agent.as_str())
            .env("AGENTD_EVENT_ID", &envelope.event_id)
            .env("AGENTD_EVENT_TYPE", &envelope.event_type)
            .env("AGENTD_STREAM_SEQUENCE", stream_sequence.to_string())
            .env("AGENTD_CONSUMER_SEQUENCE", consumer_sequence.to_string())
            .env("AGENTD_DELIVERY_COUNT", delivery_count.to_string());
        if let Some(cwd) = &config.working_directory {
            command.current_dir(cwd);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                // Spawn failure is terminal (§8.6).
                {
                    let _g = dispatch_span.enter();
                    events::handler_spawn_failed(&handler_path, &e.to_string());
                }
                self.remove_in_flight(&agent, &envelope.event_id);
                let _ = acker.term().await;
                return;
            }
        };
        let pid = child.id().unwrap_or(0);
        // Child of the dispatch span: entering it yields the full
        // `dispatch > handler` context chain on every lifecycle line (§16).
        let handler_span = logging::handler_span(&dispatch_span, &handler_path, pid);
        {
            let _h = handler_span.enter();
            events::handler_spawned(&handler_path, pid);
        }

        // Step 8: write the original bytes to stdin concurrently with
        // waiting (§8.1: events exceed the pipe buffer). EPIPE from an
        // early-exiting handler is normal.
        let write_task = spawn_stdin_writer(child.stdin.take(), raw);

        // Step 10: wait for exit. No timeout (§8.7/§15.4).
        let started = Instant::now();
        let status = child.wait().await;
        // The writer could still be blocked only if a grandchild inherited
        // the read end; abort so dispatch never wedges on it.
        write_task.abort();

        let duration_ms = started.elapsed().as_millis() as u64;
        let exit_status = match &status {
            Ok(st) => exit_status_of(st),
            Err(e) => {
                tracing::error!(agent_id = %agent, "failed to wait for handler: {e}");
                -1
            }
        };
        {
            let _h = handler_span.enter();
            events::handler_exited(exit_status, duration_ms);
        }
        if started.elapsed() > self.slow_handler_warn {
            tracing::warn!(
                agent_id = %agent,
                handler_path = %handler_path,
                duration_ms,
                "handler exceeded the slow-handler warning threshold (§15.4); not a timeout"
            );
        }

        // Step 11: record completion, then remove from in-flight. Fail-open
        // on store error: the handler already ran, so still ack (ADR-0005).
        if let Err(e) = self.dedup.mark_completed(&agent, &envelope.event_id) {
            tracing::error!(agent_id = %agent, "dedup mark failed; acking anyway: {e}");
        }
        self.remove_in_flight(&agent, &envelope.event_id);

        // Step 12: final (double) ack. On failure the record stands; the
        // redelivery dedups and re-acks (§10.5).
        if let Err(e) = acker.ack().await {
            events::ack_failure(&e);
        } else {
            events::ack_success();
        }
        // Step 13: permit released by drop.
    }

    /// In-flight dispatches for one agent (status/diagnostics).
    pub fn in_flight(&self, agent: &AgentId) -> usize {
        self.states
            .lock()
            .expect("dispatcher states poisoned")
            .get(agent)
            .map(|s| s.in_flight.len())
            .unwrap_or(0)
    }

    fn ensure_state(&self, agent: &AgentId, max_concurrency: u32) -> Arc<Semaphore> {
        let mut states = self.states.lock().expect("dispatcher states poisoned");
        states
            .entry(agent.clone())
            .or_insert_with(|| AgentState {
                permits: Arc::new(Semaphore::new(max_concurrency as usize)),
                in_flight: HashSet::new(),
            })
            .permits
            .clone()
    }

    fn remove_in_flight(&self, agent: &AgentId, event_id: &str) {
        let mut states = self.states.lock().expect("dispatcher states poisoned");
        if let Some(entry) = states.get_mut(agent) {
            entry.in_flight.remove(event_id);
        }
    }
}

/// Write the raw bytes to the handler's stdin and close it (steps 8–9).
fn spawn_stdin_writer(
    stdin: Option<impl tokio::io::AsyncWrite + Unpin + Send + 'static>,
    raw: Vec<u8>,
) -> tokio::task::JoinHandle<()> {
    let Some(mut stdin) = stdin else {
        return tokio::spawn(async {});
    };
    tokio::spawn(async move {
        if let Err(e) = stdin.write_all(&raw).await
            && e.kind() != std::io::ErrorKind::BrokenPipe
        {
            tracing::debug!("handler stdin write failed: {e}");
        }
        let _ = stdin.shutdown().await;
    })
}

/// `code()` when the process exited normally; negative signal number when
/// killed (Unix convention).
fn exit_status_of(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return -sig;
        }
    }
    -1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_status_encoding() {
        let ok = std::process::Command::new("true").status().unwrap();
        assert_eq!(exit_status_of(&ok), 0);
        let fail = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 3")
            .status()
            .unwrap();
        assert_eq!(exit_status_of(&fail), 3);
    }
}
