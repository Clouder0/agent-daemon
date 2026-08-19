# Spec 004 — Completed-event dedup store

## Goal

The minimal persistent dispatch history (whitepaper §10.2, ADR-0005): recent *completed* `(agent_id, event_id)` pairs so redeliveries skip the handler. Not an inbox — no payloads, no consumer state, nothing else.

## In scope

- `src/dedup.rs`: `DedupStore` — `open(path, ttl)`, `is_completed(agent_id, event_id)`, `mark_completed(agent_id, event_id)`, `purge_expired(ttl)`.
- SQLite via `rusqlite` (bundled); WAL + `synchronous=FULL` + `busy_timeout`; single `Mutex<Connection>`.
- Composite `(agent_id, event_id)` key (ADR-0005) — amends whitepaper §10.2 schema and §10.3 in-flight-set keying.
- Purge at open; `purge_expired` exposed for the future run loop (#2) to call periodically.
- `resolved_dedup_path()` on `DaemonConfig` (default `$XDG_DATA_HOME/agentd/dedup.db`); TTL from existing `dedup_ttl_days` (default 14 > Stream MaxAge 7).
- New error variant `AgentdError::DedupStore`.

## Out of scope

- Dispatch-time failure policy (fail-open) — lives in #3's dispatch contract, documented here and in ADR-0005 only.
- Ack sequencing (mark → double ack) — #3's contract; #4 provides primitives.
- In-flight tracking — in-memory, #3.
- Periodic re-purge — #2's run loop.

## Locked decisions (discussion 2026-08-20)

1. Composite `(agent_id, event_id)` key (ADR-0005) — user-approved Option B.
2. Store-failure policy (dispatcher-side, #3): fail-open both ways; never blocks dispatch, never amplifies duplicates.
3. `synchronous=FULL` — committed rows survive power loss; cost irrelevant at personal-domain rates.
4. Startup-only purge for v0; periodic wired later in #2.
5. Corrupt/unopenable db at startup → daemon refuses to start (clear error; operator decides).

## Acceptance criteria

- [ ] Schema exactly per ADR-0005; `CREATE TABLE IF NOT EXISTS` on open; expired rows purged at open
- [ ] `is_completed` false for unseen; true after `mark_completed`; re-mark idempotent
- [ ] Same `event_id` under different agents does not collide (the ADR-0005 regression)
- [ ] TTL purge removes only expired rows; returns count
- [ ] Concurrent access safe (single connection under mutex); file-backed open creates schema
- [ ] Unit tests cover all of the above
