# ADR-0005: Dedup keyed by (agent_id, event_id)

- **Status:** Accepted
- **Date:** 2026-08-20
- **Whitepaper sections:** §10.2, §10.3 (amended)

## Context

The whitepaper v0 draft keyed `completed_events` on `event_id` alone, trusting
§6.1's "globally unique, stable" sender obligation. But the table is shared
across all agents on the machine, while redelivery is **per-agent** — each
agent has its own durable consumer, so a redelivered copy of agent A's event
can only ever arrive on agent A's consumer.

If any adapter reuses an event_id (a hardcoded id in a hand-written script is
a realistic mistake in a self-operated domain), the global key turns that into
cross-agent interference: after the first agent completes the id, **every other
agent's event with the same id is silently skipped and acked** — undetectable
after the fact, because the schema stored no agent_id.

## Decision

Key the dedup store on the composite **(agent_id, event_id)**, and key the
dispatcher's in-flight set (ADR-0001, §10.3) the same way:

```sql
CREATE TABLE completed_events (
    agent_id     TEXT NOT NULL,
    event_id     TEXT NOT NULL,
    completed_at INTEGER NOT NULL,
    PRIMARY KEY (agent_id, event_id)
);
```

This matches the actual redelivery domain exactly: a reused event_id now
affects only the agent that received it, degrading from "silent cross-agent
event loss" to "harmless". It is strictly more correct with no additional
mechanism.

## Consequences

- Whitepaper §10.2 schema and §10.3 in-flight-set wording amended (v0.1).
- `DedupStore::is_completed` / `mark_completed` take both the agent id and the
  event id; the dispatcher (#3) keys its in-flight set by the same pair.
- Cross-agent redup suppression is not lost: redelivery never crosses
  consumers, so nothing that per-agent keying could have suppressed exists.
- Related policy (dispatcher-owned, no spec change): on dedup-store failure
  the dispatcher fails open — `is_completed` errors dispatch anyway;
  `mark_completed` errors after a handler ran still ack — so a broken store
  never blocks dispatch and never amplifies duplicates (§10.4 best-effort
  effectively-once). Both log at ERROR.
