//! Daemon-level configuration (whitepaper §7 intro, v0.1).
//!
//! Loaded from `$XDG_CONFIG_HOME/agentd/agentd.toml`. Agent registrations
//! live separately in `agents.d/` and are owned by the registry module
//! (future issue).

use std::path::Path;

use crate::error::AgentdError;

/// Daemon configuration. All fields carry whitepaper-aligned defaults;
/// every key may be overridden in the TOML file.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// NATS server URL, e.g. `nats://relay.example.internal:4222`.
    pub nats_url: String,
    /// Path to this machine's NATS credentials (mode 0600, never shared
    /// with handlers).
    pub nats_creds: Option<std::path::PathBuf>,
    /// JetStream stream name (whitepaper §5.1).
    pub stream_name: String,
    /// Control socket path; defaults to `$XDG_RUNTIME_DIR/agentd/control.sock`
    /// when unset (resolved by the daemon at startup).
    pub control_socket: Option<std::path::PathBuf>,
    /// Dedup store path; defaults under the XDG data dir when unset.
    pub dedup_path: Option<std::path::PathBuf>,
    /// Completed-event retention. Should exceed the Stream `MaxAge`
    /// (whitepaper §10.2; Stream default 7 days).
    pub dedup_ttl_days: u64,
    /// Consumer AckWait, seconds (ADR-0001: default 300 = 5m).
    pub ack_wait_secs: u64,
    /// In-progress ack interval, seconds (ADR-0001: default 90).
    pub ack_progress_interval_secs: u64,
    /// Warning threshold for long-running handlers, seconds (whitepaper
    /// §15.4 v0.1; default 3600 = 1h). Not a timeout.
    pub slow_handler_warn_secs: u64,
    /// Log level (`tracing` filter syntax).
    pub log_level: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            nats_url: "nats://127.0.0.1:4222".into(),
            nats_creds: None,
            stream_name: "AGENT_EVENTS".into(),
            control_socket: None,
            dedup_path: None,
            dedup_ttl_days: 14,
            ack_wait_secs: 300,
            ack_progress_interval_secs: 90,
            slow_handler_warn_secs: 3600,
            log_level: "info".into(),
        }
    }
}

impl DaemonConfig {
    /// Load from a TOML file. Unknown keys are rejected so typos fail loudly
    /// instead of silently being ignored.
    pub fn load(path: &Path) -> Result<Self, AgentdError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| AgentdError::config(format!("cannot read {}: {e}", path.display())))?;
        let config: DaemonConfig = toml::from_str(&raw)
            .map_err(|e| AgentdError::config(format!("invalid config {}: {e}", path.display())))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), AgentdError> {
        if self.dedup_ttl_days == 0 {
            return Err(AgentdError::config("dedup_ttl_days must be > 0".into()));
        }
        if self.ack_progress_interval_secs >= self.ack_wait_secs {
            return Err(AgentdError::config(
                "ack_progress_interval_secs must be < ack_wait_secs".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_whitepaper_and_adr_0001() {
        let c = DaemonConfig::default();
        assert_eq!(c.stream_name, "AGENT_EVENTS");
        assert_eq!(c.ack_wait_secs, 300);
        assert_eq!(c.ack_progress_interval_secs, 90);
        assert_eq!(c.slow_handler_warn_secs, 3600);
        assert!(c.dedup_ttl_days > 7, "dedup retention must exceed MaxAge");
    }

    #[test]
    fn toml_overrides_apply_and_defaults_fill_gaps() {
        let raw = r#"
            nats_url = "nats://relay.internal:4222"
            nats_creds = "/etc/agentd/relay.creds"
            ack_wait_secs = 120
        "#;
        let c: DaemonConfig = toml::from_str(raw).unwrap();
        assert_eq!(c.nats_url, "nats://relay.internal:4222");
        assert_eq!(
            c.nats_creds.as_deref(),
            Some(std::path::Path::new("/etc/agentd/relay.creds"))
        );
        assert_eq!(c.ack_wait_secs, 120);
        assert_eq!(c.stream_name, "AGENT_EVENTS");
        assert_eq!(c.ack_progress_interval_secs, 90);
        c.validate().unwrap();
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let raw = "nats_urk = \"typo\"";
        assert!(toml::from_str::<DaemonConfig>(raw).is_err());
    }

    #[test]
    fn progress_interval_must_be_shorter_than_ack_wait() {
        let raw = "ack_wait_secs = 60";
        let c: DaemonConfig = toml::from_str(raw).unwrap();
        assert!(c.validate().is_err());
    }
}
