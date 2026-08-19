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
    /// Directory of per-agent registration files (`agents.d`). Defaults to
    /// `$XDG_CONFIG_HOME/agentd/agents.d` when unset; missing dir = empty
    /// registry, created on first write.
    pub agents_dir: Option<std::path::PathBuf>,
    /// Control socket path; defaults to `$XDG_RUNTIME_DIR/agentd/control.sock`
    /// when unset (resolved by the daemon at startup).
    pub control_socket: Option<std::path::PathBuf>,
    /// Dedup store path; defaults under the XDG data dir when unset.
    pub dedup_path: Option<std::path::PathBuf>,
    /// Maximum accepted event size in bytes (whitepaper §5.1 default 256 KiB,
    /// §15.2 terminal). Enforced where the relay hands the dispatcher bytes.
    pub max_event_bytes: u64,
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
            agents_dir: None,
            control_socket: None,
            dedup_path: None,
            max_event_bytes: 256 * 1024,
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
        if self.max_event_bytes == 0 {
            return Err(AgentdError::config("max_event_bytes must be > 0".into()));
        }
        if self.ack_progress_interval_secs >= self.ack_wait_secs {
            return Err(AgentdError::config(
                "ack_progress_interval_secs must be < ack_wait_secs".into(),
            ));
        }
        Ok(())
    }

    /// The agents directory, honoring `$XDG_CONFIG_HOME` when unset
    /// (whitepaper §7.4). Used by the registry; config stays an `Option`
    /// so the file format and defaults are distinct.
    pub fn resolved_agents_dir(&self) -> std::path::PathBuf {
        self.agents_dir.clone().unwrap_or_else(|| {
            let base = dirs::config_dir().unwrap_or_default();
            base.join("agentd").join("agents.d")
        })
    }

    /// The dedup store path, honoring `$XDG_DATA_HOME` when unset
    /// (whitepaper §10.2). A corrupt or unopenable store is a startup
    /// error, not silently discarded history.
    pub fn resolved_dedup_path(&self) -> std::path::PathBuf {
        self.dedup_path.clone().unwrap_or_else(|| {
            let base = dirs::data_dir().unwrap_or_default();
            base.join("agentd").join("dedup.db")
        })
    }

    /// The dedup TTL as a `Duration` (from `dedup_ttl_days`).
    pub fn dedup_ttl(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.dedup_ttl_days * 24 * 3600)
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
        assert_eq!(c.max_event_bytes, 256 * 1024, "whitepaper §5.1 default");
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
    fn agents_dir_resolves_explicit_or_xdg_default() {
        // explicit value wins
        let explicit = DaemonConfig {
            agents_dir: Some(std::path::PathBuf::from("/opt/agents.d")),
            ..DaemonConfig::default()
        };
        assert_eq!(
            explicit.resolved_agents_dir(),
            std::path::PathBuf::from("/opt/agents.d")
        );

        // unset → $XDG_CONFIG_HOME/agentd/agents.d (via dirs)
        let unset = DaemonConfig::default();
        let resolved = unset.resolved_agents_dir();
        assert!(
            resolved.ends_with("agentd/agents.d"),
            "expected an agentd/agents.d suffix, got {resolved:?}"
        );
    }

    #[test]
    fn progress_interval_must_be_shorter_than_ack_wait() {
        let raw = "ack_wait_secs = 60";
        let c: DaemonConfig = toml::from_str(raw).unwrap();
        assert!(c.validate().is_err());
    }

    #[test]
    fn dedup_path_resolves_explicit_or_xdg_default() {
        let explicit = DaemonConfig {
            dedup_path: Some(std::path::PathBuf::from("/var/lib/agentd/dedup.db")),
            ..DaemonConfig::default()
        };
        assert_eq!(
            explicit.resolved_dedup_path(),
            std::path::PathBuf::from("/var/lib/agentd/dedup.db")
        );

        let unset = DaemonConfig::default();
        let resolved = unset.resolved_dedup_path();
        assert!(
            resolved.ends_with("agentd/dedup.db"),
            "expected an agentd/dedup.db suffix, got {resolved:?}"
        );
    }

    #[test]
    fn dedup_ttl_derives_from_days() {
        let c = DaemonConfig::default();
        assert_eq!(
            c.dedup_ttl(),
            std::time::Duration::from_secs(14 * 24 * 3600)
        );
    }
}
