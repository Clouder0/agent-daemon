//! Unified error taxonomy (skeleton; grows with the modules that use it).

#[derive(Debug, thiserror::Error)]
pub enum AgentdError {
    #[error("invalid agent id: {0}")]
    InvalidAgentId(String),

    #[error("invalid event envelope: {0}")]
    InvalidEnvelope(String),

    #[error("configuration error: {0}")]
    Config(String),
}

impl AgentdError {
    pub(crate) fn invalid_agent_id(reason: String) -> Self {
        Self::InvalidAgentId(reason)
    }

    pub(crate) fn invalid_envelope(reason: String) -> Self {
        Self::InvalidEnvelope(reason)
    }

    pub(crate) fn config(reason: String) -> Self {
        Self::Config(reason)
    }
}

/// Convenient result alias used across the crate.
pub type Result<T, E = AgentdError> = std::result::Result<T, E>;
