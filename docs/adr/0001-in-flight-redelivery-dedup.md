# ADR-0001: In-flight redelivery deduplication

- **Status:** Accepted
- **Date:** 2026-08-19 (amended into whitepaper v0.1 on 2026-08-20)
- **Whitepaper sections:** §8.1, §10.3

## Context

The whitepaper v0 draft documented exactly one duplicate window: a crash between Handler side effects and the `completed_events` write. Design review found a second path that requires **no crash**:

1. `agentd` pulls a message and spawns a Handler (long-running).
2. In-progress acks are the only thing preventing redelivery, and they work only while `agentd` can reach the server.
3. The machine suspends (laptop) or the network partitions for longer than `AckWait`. Server-side, the AckWait clock keeps running.
4. On resume/reconnect, JetStream redelivers the still-unacked message to the same `agentd`, while the original Handler is still running.
5. The completed-events dedup store misses (the Handler has not exited); with `max_concurrency > 1` and a free slot, a second Handler for the same event runs concurrently — duplicate side effects with no crash.

Since the stated platform target includes laptops and desktops (whitepaper §14.1), suspend/resume is a mainstream scenario, not an edge case.

## Decision

`agentd` maintains an **in-memory set of in-flight `event_id`s**:

- Pulls are gated by free concurrency slots; `agentd` never holds more messages than it can currently dispatch.
- The completed-dedup check happens at spawn-decision time (after acquiring the slot), not only at receive time.
- When a copy of an event that is already in-flight arrives, `agentd` drops the local copy **without acking**. The server redelivers after `AckWait`; by then the first dispatch has completed, the completed-store dedup hits, and `agentd` acks.
- Defaults: `AckWait = 5m`, in-progress acks every `90s` (both configurable).
- §21.2 gains an "in-flight redelivery" test (freeze `agentd` / drop in-progress acks beyond `AckWait`).

## Consequences

- The rare duplicate-while-in-flight path costs one extra `AckWait`-scale delay; no new persisted state; no lock contention.
- On machine crash the in-flight set is lost and behavior degrades to the already-documented crash window (§10.4) — deliberately, since persisting spawn-marks would convert a possible duplicate into a possible *lost* event.
- Handlers doing critical side effects must still be idempotent via `event_id` (unchanged requirement).
