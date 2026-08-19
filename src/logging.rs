//! Structured logging (whitepaper §16): JSON lines on stdout for the daemon.
//!
//! The [`events`] submodule is the single home for the §16 must-log list —
//! every module logs through these helpers so the field vocabulary stays
//! consistent. Credential *contents* are never logged (structurally: no
//! helper accepts credential data; paths only).

use tracing_subscriber::EnvFilter;

use crate::agent_id::AgentId;
use crate::config::DaemonConfig;
use crate::error::AgentdError;

/// Initialize the daemon's global tracing subscriber: JSON lines to stdout.
///
/// Filter precedence: a parseable `RUST_LOG` overrides the configured
/// `log_level`; an invalid `RUST_LOG` is reported to stderr and falls back;
/// an invalid configured level is a configuration error.
pub fn init(config: &DaemonConfig) -> Result<(), AgentdError> {
    let filter = build_filter(std::env::var("RUST_LOG").ok().as_deref(), &config.log_level)?;
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_env_filter(filter)
        .with_writer(std::io::stdout)
        .try_init()
        .map_err(|e| AgentdError::config(format!("failed to install tracing subscriber: {e}")))?;
    Ok(())
}

/// Build the log filter. Pure and unit-testable: `init` only contributes the
/// environment lookup.
pub(crate) fn build_filter(
    env: Option<&str>,
    config_level: &str,
) -> Result<EnvFilter, AgentdError> {
    if let Some(raw) = env.map(str::trim).filter(|s| !s.is_empty()) {
        match EnvFilter::try_new(raw) {
            Ok(filter) => return Ok(filter),
            Err(e) => eprintln!("agentd: ignoring invalid RUST_LOG={raw:?}: {e}"),
        }
    }
    EnvFilter::try_new(config_level)
        .map_err(|e| AgentdError::config(format!("invalid log_level filter {config_level:?}: {e}")))
}

/// The per-dispatch span: every event emitted while entered inherits the
/// §16 per-event fields, so helpers do not repeat them.
pub fn dispatch_span(
    agent_id: &AgentId,
    event_id: &str,
    consumer: &str,
    stream_sequence: u64,
) -> tracing::Span {
    tracing::info_span!(
        "dispatch",
        agent_id = %agent_id,
        event_id,
        consumer,
        stream_sequence,
    )
}

/// One helper per whitepaper §16 must-log event. Emission is wired by the
/// modules that own each lifecycle (#2 relay, #5 registry, #3 dispatcher).
pub mod events {
    use crate::agent_id::AgentId;

    // Relay lifecycle (#2).
    pub fn nats_connected(url: &str) {
        tracing::info!(nats_url = url, "nats connected");
    }

    pub fn nats_disconnected() {
        tracing::warn!("nats disconnected");
    }

    // Registry lifecycle (#5).
    pub fn agent_registered(agent_id: &AgentId) {
        tracing::info!(agent_id = %agent_id, "agent registered");
    }

    pub fn agent_updated(agent_id: &AgentId) {
        tracing::info!(agent_id = %agent_id, "agent updated");
    }

    pub fn agent_unregistered(agent_id: &AgentId) {
        tracing::info!(agent_id = %agent_id, "agent unregistered");
    }

    // Consumer lifecycle (#2).
    pub fn consumer_bound(agent_id: &AgentId, consumer: &str) {
        tracing::info!(agent_id = %agent_id, consumer, "consumer bound");
    }

    // Dispatch lifecycle (#3). agent_id/event_id/sequences arrive via the
    // dispatch span; only event-specific fields are passed here.
    pub fn event_received() {
        tracing::info!("event received");
    }

    pub fn dedup_hit() {
        tracing::info!("dedup hit");
    }

    pub fn in_flight_duplicate_dropped() {
        tracing::warn!("in-flight duplicate dropped");
    }

    pub fn invalid_event(reason: &str) {
        tracing::warn!(reason, "invalid event");
    }

    pub fn handler_spawned(handler_path: &str, pid: u32) {
        tracing::info!(handler_path, handler_pid = pid, "handler spawned");
    }

    pub fn handler_spawn_failed(handler_path: &str, error: &str) {
        tracing::warn!(handler_path, error, "handler spawn failed");
    }

    /// `exit_status` encoding for signal deaths is decided by the
    /// dispatcher (#3); logging only passes it through.
    pub fn handler_exited(exit_status: i32, duration_ms: u64) {
        tracing::info!(exit_status, duration_ms, "handler exited");
    }

    // Acks (#2/#3).
    pub fn ack_success() {
        tracing::info!("ack succeeded");
    }

    pub fn ack_failure(error: &str) {
        tracing::warn!(error, "ack failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::field::Visit;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::layer::Layer;
    use tracing_subscriber::prelude::*;

    // -- filter precedence --------------------------------------------------

    fn debug_enabled(filter: EnvFilter) -> bool {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::sink)
            .finish();
        tracing::dispatcher::with_default(&tracing::Dispatch::new(subscriber), || {
            tracing::enabled!(tracing::Level::DEBUG)
        })
    }

    #[test]
    fn env_filter_overrides_config_level() {
        let f = build_filter(Some("debug"), "info").unwrap();
        assert!(debug_enabled(f));
    }

    #[test]
    fn config_level_used_when_env_unset() {
        let f = build_filter(None, "info").unwrap();
        assert!(!debug_enabled(f));
        let f = build_filter(Some("  "), "debug").unwrap(); // blank env = unset
        assert!(debug_enabled(f));
    }

    #[test]
    fn invalid_env_falls_back_to_config() {
        let f = build_filter(Some("===not a filter==="), "info").unwrap();
        assert!(!debug_enabled(f));
    }

    #[test]
    fn invalid_config_level_is_an_error() {
        assert!(build_filter(None, "===not a filter===").is_err());
    }

    // -- field capture ------------------------------------------------------

    /// Records field names and values as `name=value` strings.
    struct Fields(Vec<String>);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push(format!("{}={value:?}", field.name()));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push(format!("{}={value}", field.name()));
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.0.push(format!("{}={value}", field.name()));
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.0.push(format!("{}={value}", field.name()));
        }
    }

    /// Capturing harness: span fields at creation, events as `spans>fields`.
    /// (`SpanRef` exposes no field visitor, so fields are captured in
    /// `on_new_span` and joined by span id at event time.)
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<CaptureState>>);

    #[derive(Default)]
    struct CaptureState {
        spans: std::collections::HashMap<u64, String>,
        events: Vec<String>,
    }

    impl<S> Layer<S> for Capture
    where
        S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            id: &tracing::span::Id,
            _ctx: Context<'_, S>,
        ) {
            let mut fields = Fields(vec![]);
            attrs.record(&mut fields);
            self.0.lock().unwrap().spans.insert(
                id.into_u64(),
                format!("{}[{}]", attrs.metadata().name(), fields.0.join(",")),
            );
        }

        fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
            let mut fields = Fields(vec![]);
            event.record(&mut fields);
            let state = self.0.lock().unwrap();
            let mut scope = vec![];
            if let Some(spans) = ctx.event_scope(event) {
                for span in spans {
                    scope.push(
                        state
                            .spans
                            .get(&span.id().into_u64())
                            .cloned()
                            .unwrap_or_else(|| span.name().to_string()),
                    );
                }
            }
            let line = format!("{} {}", scope.join(">"), fields.0.join(","));
            let mut state = state;
            state.events.push(line);
        }
    }

    #[test]
    fn dispatch_span_fields_attach_to_nested_events() {
        let capture = Capture::default();
        let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(capture.clone()));
        tracing::dispatcher::with_default(&dispatch, || {
            let agent = AgentId::parse("coding/main").unwrap();
            let span = dispatch_span(&agent, "ev-1", "agent-abc", 42);
            let _guard = span.enter();
            events::handler_exited(0, 214);
        });
        let lines = &capture.0.lock().unwrap().events;
        assert_eq!(lines.len(), 1, "one event captured: {lines:?}");
        assert!(
            lines[0].contains(
                "dispatch[agent_id=coding/main,event_id=ev-1,consumer=agent-abc,stream_sequence=42]"
            ),
            "span fields missing: {lines:?}"
        );
        assert!(
            lines[0].contains("exit_status=0") && lines[0].contains("duration_ms=214"),
            "event fields missing: {lines:?}"
        );
    }

    #[test]
    fn helpers_compile_and_emit() {
        let capture = Capture::default();
        let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(capture.clone()));
        tracing::dispatcher::with_default(&dispatch, || {
            let agent = AgentId::parse("assistant/personal").unwrap();
            events::nats_connected("nats://127.0.0.1:4222");
            events::agent_registered(&agent);
            events::consumer_bound(&agent, "agent-xyz");
            events::event_received();
            events::dedup_hit();
            events::in_flight_duplicate_dropped();
            events::invalid_event("unsupported version");
            events::handler_spawned("/bin/true", 1234);
            events::handler_spawn_failed("/bin/true", "no such file");
            events::ack_success();
            events::ack_failure("timeout");
        });
        assert_eq!(capture.0.lock().unwrap().events.len(), 11);
    }
}
