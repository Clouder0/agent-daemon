//! Event Envelope v0 (whitepaper §6).
//!
//! `agentd` interprets only `version`, `event_id`, and `agent_id`
//! (§6.3). Everything else is passed through to the Handler unchanged —
//! including unknown fields, which must never cause rejection.
//!
//! The dispatcher hands Handlers the *original* message bytes, so this type is
//! for validation and routing only, not for re-serialization.

use crate::agent_id::AgentId;
use crate::error::AgentdError;

/// The only Envelope version v0 accepts (whitepaper §6.1).
pub const ENVELOPE_VERSION: u32 = 1;

/// A parsed Event Envelope. `payload` and `metadata` are opaque pass-through.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct EventEnvelope {
    pub version: u32,
    pub event_id: String,
    pub agent_id: AgentId,
    #[serde(rename = "type")]
    pub event_type: String,
    pub created_at: String,
    pub payload: serde_json::Value,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

impl EventEnvelope {
    /// Parse from raw bytes and apply the envelope-level validation `agentd`
    /// is allowed to perform: JSON parses, `version` is supported,
    /// `event_id` is non-empty (whitepaper §15.2).
    pub fn parse(bytes: &[u8]) -> Result<Self, AgentdError> {
        let envelope: EventEnvelope = serde_json::from_slice(bytes)
            .map_err(|e| AgentdError::invalid_envelope(format!("unparseable JSON: {e}")))?;
        envelope.validate()?;
        Ok(envelope)
    }

    fn validate(&self) -> Result<(), AgentdError> {
        if self.version != ENVELOPE_VERSION {
            return Err(AgentdError::invalid_envelope(format!(
                "unsupported version {} (only {ENVELOPE_VERSION})",
                self.version
            )));
        }
        if self.event_id.trim().is_empty() {
            return Err(AgentdError::invalid_envelope("missing event_id".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{
        "version": 1,
        "event_id": "01J6ZP8R5EF4Y42KABCD123456",
        "agent_id": "coding_main",
        "type": "im.message",
        "created_at": "2026-08-19T12:00:00Z",
        "payload": {"text": "please continue"},
        "metadata": {"source": "matrix", "sender": "@alice:domain.test"}
    }"#;

    #[test]
    fn parses_whitepaper_example() {
        let env = EventEnvelope::parse(EXAMPLE.as_bytes()).unwrap();
        assert_eq!(env.event_id, "01J6ZP8R5EF4Y42KABCD123456");
        assert_eq!(env.agent_id.to_string(), "coding_main");
        assert_eq!(env.event_type, "im.message");
        assert_eq!(
            env.metadata.as_ref().unwrap()["source"],
            serde_json::json!("matrix")
        );
    }

    #[test]
    fn unknown_fields_are_accepted() {
        let with_extra = EXAMPLE.replacen(
            "\"version\": 1,",
            "\"version\": 1, \"future_field\": {\"nested\": [1, 2]},",
            1,
        );
        assert!(EventEnvelope::parse(with_extra.as_bytes()).is_ok());
    }

    #[test]
    fn unsupported_version_is_terminal() {
        let v2 = EXAMPLE.replace("\"version\": 1,", "\"version\": 2,");
        let err = EventEnvelope::parse(v2.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("unsupported version"));
    }

    #[test]
    fn missing_event_id_is_terminal() {
        let missing = EXAMPLE.replacen("\"event_id\": \"01J6ZP8R5EF4Y42KABCD123456\",", "", 1);
        assert!(EventEnvelope::parse(missing.as_bytes()).is_err());
    }

    #[test]
    fn empty_event_id_is_terminal() {
        let empty = EXAMPLE.replace(
            "\"event_id\": \"01J6ZP8R5EF4Y42KABCD123456\"",
            "\"event_id\": \"\"",
        );
        assert!(EventEnvelope::parse(empty.as_bytes()).is_err());
    }

    #[test]
    fn invalid_agent_id_is_terminal() {
        let bad = EXAMPLE.replace("\"agent_id\": \"coding_main\"", "\"agent_id\": \"Bad Id\"");
        assert!(EventEnvelope::parse(bad.as_bytes()).is_err());
    }

    #[test]
    fn unparseable_json_is_terminal() {
        assert!(EventEnvelope::parse(b"{ not json").is_err());
    }
}
