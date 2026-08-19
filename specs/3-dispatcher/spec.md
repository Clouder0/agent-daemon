# Spec 003 — Event dispatcher

## Goal

The heart of the daemon (whitepaper §8, ADR-0001/0005): turn one relay delivery into one handler invocation with correct dedup, concurrency, and terminal-event semantics. Never retries anything.

## In scope

- `src/dispatcher.rs`: `Dispatcher`, `Delivery`, `Acker` trait (relay seam), `DedupCheck` trait (fail-open testability), per-agent `AgentState` (semaphore + in-flight set).
- The §8.1 v0.1 13-step sequence exactly: size gate → parse/validate → registration check → slot → completed-dedup → atomic in-flight check-and-insert → spawn (execve, §8.3 env vars, cwd) → original bytes to stdin (parallel write; EPIPE normal; write task aborted on child exit) → wait → exit recording + slow warn → mark completed → in-flight removal → double ack.
- `available(agent_id) -> usize` so the relay (#2) pulls by free slots.
- `apply_changes(&[Change])` for registry reloads (Added/Updated resize; Removed/Disabled stop new dispatch; in-flight drains naturally).
- Fail-open policy (ADR-0005): `is_completed` error → dispatch anyway; `mark_completed` error → still ack. Both ERROR-logged.

## Out of scope

- Real acks (async-nats, #2); run loop and shutdown wiring (#2 uses the primitives); `agentd.toml` loading (#2).

## Locked decisions (2026-08-20)

1. `Delivery<A: Acker>` generic — no dyn, no async-trait dep.
2. Dedup behind 2-method trait; `DedupStore` implements it.
3. Per-agent state map; in-flight keyed by event_id within the agent.
4. Unregistered/disabled-agent delivery → log + term.
5. `exit_status = code.unwrap_or(-signal)`.

Defaults taken: spawn failure → term; concurrency change → natural drain; write task aborted after child exit (grandchild holding stdin cannot wedge dispatch).

## Acceptance criteria

Issue #3 checklist, verified by integration tests against fake handler scripts: stdin byte-equality, env vars, serial (`=1`) and parallel (`=3`) gates, exit-1 single-ack, spawn-failure term, invalid envelope term, oversize term, dedup-hit skip+ack, in-flight duplicate dropped without ack, EPIPE early-exit, fail-open both ways, unregistered term.
