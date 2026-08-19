# Spec 006 — Control plane (socket + agentdctl)

## Goal

The local control surface (whitepaper §7.3, §18): a Unix control socket
serving register/update/unregister/list/reload/status, and `agentdctl`
driving it (plus operator-time `init`).

## Locked decisions (2026-08-20)

1. One apply path: `DaemonHandle` shared by SIGHUP and every control op —
   a socket `register` binds the consumer immediately.
2. `RelayBackend` seam (apply_changes + consumer_backlog) — Relay
   implements; tests fake it (same pattern as Acker/DedupCheck).
3. status v0: connected, per-agent config/in-flight/backlog (best effort);
   last-event/last-error deferred (documented §16 deviation).
4. `agentdctl init` takes `--creds`/`--url` overrides (operator creds ≠
   daemon creds, v0.1); other ops take `--socket` (or config-resolved).
5. Socket: XDG runtime default, 0600, stale removal at bind, cleanup at
   shutdown.

## Acceptance

- [x] Socket ops work over a real UDS; malformed input errors, no disconnect
- [x] register persists + applies Added; update/unregister flow; reload diff
- [x] status reports agents + backlog; mode 0600 verified
- [x] agentdctl: init against live server; register/list/status/update/
      unregister human output
- [x] Live smoke: register-while-running → dispatch without restart;
      disable stops consuming; graceful stop cleans the socket
- [x] 72/72 tests; lint clean
