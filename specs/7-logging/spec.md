# Spec 007 — Structured logging

## Goal

The daemon's observability surface (whitepaper §16): JSON structured logs with a fixed field vocabulary, one helper per must-log event, and per-dispatch span context. `agentdctl` stays human-readable.

**Re-scope (2026-08-20):** `agentdctl status` moved to #6 — it needs the control socket (#6) and relay/dispatcher data (#2/#3) that don't exist yet.

## In scope

- `src/logging.rs`: subscriber init (JSON lines to stdout; `RUST_LOG` overrides configured `log_level`), filter-precedence logic as a testable pure function.
- `events` submodule: one function per §16 must-log event (incl. v0.1 `in_flight_duplicate_dropped`); field names live only here.
- `dispatch_span(agent_id, event_id, consumer, stream_sequence)`: all events emitted while entered inherit the §16 per-event fields.
- Wiring into `agentd run` (init first; the not-implemented warning goes through tracing).

## Out of scope

- `agentdctl status` (#6); emission of relay/registry/dispatcher events (wired by #2–#6 as they land); handler stdout/stderr forwarding (inherited per §8.4 — decided in discussion).
- New dependencies: none (`tracing` + `tracing-subscriber` already declared).

## Locked decisions (discussion 2026-08-20)

1. JSON lines to stdout for `agentd`; plain stderr for `agentdctl` (no subscriber).
2. Handler stdout/stderr inherited, not piped; `handler_pid` distinguishes them in the journal.
3. Filter precedence: valid `RUST_LOG` > config `log_level`; invalid `RUST_LOG` warns on stderr and falls back; invalid config level is a config error.
4. Credential contents are never logged — structurally: no helper accepts credential data.

## Acceptance criteria

- [ ] `agentd run` emits JSON log lines on stdout; `RUST_LOG` overrides the configured level.
- [ ] Every §16 must-log event has a helper in `logging::events` (incl. in-flight duplicate dropped).
- [ ] `dispatch_span` attaches agent_id / event_id / consumer / stream_sequence to nested events — verified by test.
- [ ] No helper takes credential contents; no credential data appears in any log call.
