//! agent-daemon library core.
//!
//! Semantics are defined by `docs/whitepaper-v0.md` (source of truth). If code
//! and whitepaper disagree, the whitepaper wins until amended by PR.

pub mod agent_id;
pub mod config;
pub mod error;
pub mod event;
pub mod logging;
