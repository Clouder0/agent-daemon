# Spec 002 — Relay + run assembly

## Goal

The async-nats half (whitepaper §5, §15.1, §10.5) and the `agentd run`
assembly that finally makes the daemon real: connection, per-agent durable
pull consumers, slot-driven pulls, double-ack/term, in-progress keepalive,
SIGHUP reload, graceful shutdown.

## Locked decisions (2026-08-20 discussion)

1. **ADR-0006**: unified `_` agent ids — the id is a single NATS token; id ==
   subject tail == consumer name == filename, every mapping an identity;
   consumer name `agent-<id>` verbatim; no hashing.
2. Pull strategy: `batch(max_messages=available)` long-poll 30s when slots
   are free; 1s sleep when none.
3. Reload: startup + SIGHUP (until #6's control socket).
4. `ensure_stream()` lives here; #6's `agentdctl init` calls it.
5. Keepalive: per-dispatch ticker task (90s cadence), aborted at dispatch end.
6. Shutdown: full §14.3 graceful (drain dispatch tasks incl. acks; systemd is
   the backstop). `dispatch_tasks` is a tokio Mutex (held across join).

## In scope

- `src/relay.rs`: `connect` (creds, retry-on-initial-connect), `ensure_stream`
  (operator-time), `consumer_name`/`consumer_config` (§5.3 + ADR-0001),
  `NatsAcker` (double ack / term), `Relay` (bind, slot-driven pull loops,
  `apply_changes`, `shutdown`), in-progress keepalive.
- `main.rs` `agentd run`: `--config` (default `$XDG_CONFIG_HOME/agentd/agentd.toml`;
  missing default → defaults + note), logging init, dedup open (corrupt → refuse
  start), registry load, dispatcher, relay sync, SIGHUP reload loop, SIGTERM/Ctrl-C
  graceful stop. Per-agent bind failures log and skip (one broken agent never
  takes down the rest).

## Out of scope

Real-server matrix (offline delivery, reconnect, ack-loss, crash window) — #9.
Control socket / `agentdctl` — #6.

## Acceptance

- [x] Unit: consumer name/config per §5.3 + ADR-0001; identity mappings
- [x] 65/65 tests; lint clean
- [x] Live smoke vs dockerized nats-server: init stream, register agent,
      publish, handler receives exact bytes, consumer visible, SIGTERM drains
- [x] ADR-0006 + full `_` sweep (whitepaper EN/zh, code, tests, README)
