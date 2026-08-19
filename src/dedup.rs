//! Completed-event dedup store (whitepaper §10.2, ADR-0005).
//!
//! Minimal persistent dispatch history: recent *completed* `(agent_id,
//! event_id)` pairs so redeliveries skip the handler. It is not an inbox —
//! no payloads, no consumer state, nothing else.
//!
//! The composite key (ADR-0005) matches the redelivery domain: redelivery is
//! per-agent (one durable consumer per agent), so a reused event_id from a
//! buggy sender affects only the agent that received it.
//!
//! This module is a pure primitive. The dispatcher (#3) owns sequencing —
//! `mark_completed` after handler exit, then the final double ack — and the
//! failure policy: on store errors it fails open (dispatch anyway; ack after
//! a mark failure), so a broken store never blocks dispatch and never
//! amplifies duplicates (§10.4 best-effort effectively-once).

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::agent_id::AgentId;
use crate::error::AgentdError;

/// The dedup store: one SQLite connection under a mutex. Queries are
/// microsecond-scale at personal-domain dispatch rates, so a single
/// serialized connection is ample; `synchronous=FULL` makes a committed row
/// survive power loss, which the dedup guarantee depends on (§10.4).
pub struct DedupStore {
    conn: Mutex<Connection>,
}

impl DedupStore {
    /// Open (or create) the file-backed store, apply pragmas and schema, and
    /// purge rows older than `ttl`. A corrupt or unopenable database is an
    /// error — the caller (daemon startup) refuses to start rather than
    /// silently discarding dedup history.
    pub fn open(path: &Path, ttl: Duration) -> Result<Self, AgentdError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentdError::dedup_store(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        let conn = Connection::open(path).map_err(|e| {
            AgentdError::dedup_store(format!("cannot open {}: {e}", path.display()))
        })?;
        let store = Self::with_connection(conn)?;
        store.purge_expired(ttl)?;
        Ok(store)
    }

    /// An in-memory store (tests).
    pub fn open_in_memory() -> Result<Self, AgentdError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| AgentdError::dedup_store(format!("cannot open in-memory store: {e}")))?;
        Self::with_connection(conn)
    }

    fn with_connection(conn: Connection) -> Result<Self, AgentdError> {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| AgentdError::dedup_store(format!("cannot set journal_mode=WAL: {e}")))?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(|e| AgentdError::dedup_store(format!("cannot set synchronous=FULL: {e}")))?;
        conn.pragma_update(None, "busy_timeout", 5000)
            .map_err(|e| AgentdError::dedup_store(format!("cannot set busy_timeout: {e}")))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS completed_events (
                 agent_id     TEXT NOT NULL,
                 event_id     TEXT NOT NULL,
                 completed_at INTEGER NOT NULL,
                 PRIMARY KEY (agent_id, event_id)
             );",
        )
        .map_err(|e| AgentdError::dedup_store(format!("cannot create schema: {e}")))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Whether this agent's event has already completed. Called at dispatch
    /// decision time; a redelivery of a completed event skips the handler.
    pub fn is_completed(&self, agent_id: &AgentId, event_id: &str) -> Result<bool, AgentdError> {
        let conn = self.conn.lock().expect("dedup store lock poisoned");
        let found: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM completed_events WHERE agent_id = ?1 AND event_id = ?2)",
                params![agent_id.as_str(), event_id],
                |row| row.get(0),
            )
            .map_err(|e| AgentdError::dedup_store(format!("is_completed lookup failed: {e}")))?;
        Ok(found)
    }

    /// Record completion. Idempotent (`INSERT OR IGNORE`). The dispatcher
    /// calls this after the handler exits and before the final double ack
    /// (§10.2, §10.5).
    pub fn mark_completed(&self, agent_id: &AgentId, event_id: &str) -> Result<(), AgentdError> {
        let now = unix_now();
        let conn = self.conn.lock().expect("dedup store lock poisoned");
        conn.execute(
            "INSERT OR IGNORE INTO completed_events (agent_id, event_id, completed_at)
             VALUES (?1, ?2, ?3)",
            params![agent_id.as_str(), event_id, now],
        )
        .map_err(|e| AgentdError::dedup_store(format!("mark_completed insert failed: {e}")))?;
        Ok(())
    }

    /// Delete rows older than `ttl`; returns the number purged. Run at
    /// startup (`open`); the run loop (#2) will call it periodically.
    pub fn purge_expired(&self, ttl: Duration) -> Result<u64, AgentdError> {
        let cutoff = unix_now().saturating_sub(ttl.as_secs());
        let conn = self.conn.lock().expect("dedup store lock poisoned");
        let purged = conn
            .execute(
                "DELETE FROM completed_events WHERE completed_at < ?1",
                params![cutoff],
            )
            .map_err(|e| AgentdError::dedup_store(format!("purge failed: {e}")))?;
        Ok(purged as u64)
    }

    /// Row count (diagnostics/tests).
    pub fn len(&self) -> Result<u64, AgentdError> {
        let conn = self.conn.lock().expect("dedup store lock poisoned");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM completed_events", [], |row| {
                row.get(0)
            })
            .map_err(|e| AgentdError::dedup_store(format!("count failed: {e}")))?;
        Ok(n as u64)
    }

    /// Always false when rows exist (clippy).
    pub fn is_empty(&self) -> Result<bool, AgentdError> {
        Ok(self.len()? == 0)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn agent(s: &str) -> AgentId {
        AgentId::parse(s).unwrap()
    }

    #[test]
    fn mark_then_hit_and_idempotent_remark() {
        let s = DedupStore::open_in_memory().unwrap();
        let a = agent("coding.main");
        assert!(!s.is_completed(&a, "e1").unwrap());
        s.mark_completed(&a, "e1").unwrap();
        assert!(s.is_completed(&a, "e1").unwrap());
        // Re-marking is idempotent: still exactly one row.
        s.mark_completed(&a, "e1").unwrap();
        assert_eq!(s.len().unwrap(), 1);
    }

    /// The ADR-0005 regression: a reused event_id under a different agent
    /// must NOT collide.
    #[test]
    fn same_event_id_different_agents_do_not_collide() {
        let s = DedupStore::open_in_memory().unwrap();
        let a = agent("coding.main");
        let b = agent("assistant.personal");
        s.mark_completed(&a, "reused-id").unwrap();
        assert!(s.is_completed(&a, "reused-id").unwrap());
        assert!(
            !s.is_completed(&b, "reused-id").unwrap(),
            "agent B's event with the same id must not be suppressed"
        );
        s.mark_completed(&b, "reused-id").unwrap();
        assert_eq!(s.len().unwrap(), 2);
    }

    #[test]
    fn ttl_purge_removes_only_expired_rows() {
        let s = DedupStore::open_in_memory().unwrap();
        let a = agent("a");
        s.mark_completed(&a, "old").unwrap();

        // `completed_at` has 1-second granularity: ttl=0 only expires rows
        // from a strictly earlier second, so cross the boundary for real.
        std::thread::sleep(Duration::from_millis(1100));
        let purged = s.purge_expired(Duration::from_secs(0)).unwrap();
        assert_eq!(purged, 1);
        assert!(s.is_empty().unwrap(), "expired row should be gone");
    }

    #[test]
    fn ttl_purge_keeps_recent_rows() {
        let s = DedupStore::open_in_memory().unwrap();
        let a = agent("a");
        s.mark_completed(&a, "recent").unwrap();
        let purged = s.purge_expired(Duration::from_secs(3600)).unwrap();
        assert_eq!(purged, 0, "nothing should expire within the ttl");
        assert_eq!(s.len().unwrap(), 1);
    }

    #[test]
    fn concurrent_access_is_serialized() {
        let s = Arc::new(DedupStore::open_in_memory().unwrap());
        let mut handles = Vec::new();
        for t in 0..4 {
            let s = s.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..100 {
                    let a = agent("race.agent");
                    s.mark_completed(&a, &format!("e-{t}-{i}")).unwrap();
                    assert!(s.is_completed(&a, &format!("e-{t}-{i}")).unwrap());
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(s.len().unwrap(), 400);
    }

    #[test]
    fn file_backed_open_creates_schema_and_purges() {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "agentd-dedup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = dir.join("dedup.db");
        {
            let s = DedupStore::open(&db, Duration::from_secs(3600)).unwrap();
            s.mark_completed(&agent("a"), "e1").unwrap();
        }
        // Reopen: schema persists, startup purge keeps recent rows.
        let s2 = DedupStore::open(&db, Duration::from_secs(3600)).unwrap();
        assert!(s2.is_completed(&agent("a"), "e1").unwrap());
        // Startup purge runs: with a crossed second boundary, ttl=0 clears all.
        std::thread::sleep(Duration::from_millis(1100));
        let s3 = DedupStore::open(&db, Duration::from_secs(0)).unwrap();
        assert!(s3.is_empty().unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
