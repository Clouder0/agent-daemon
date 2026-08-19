# Plan 007 — Structured logging

- **Status:** Approved (design settled in discussion 2026-08-20; recorded here, not re-opened)
- **Issue:** #7 (`Closes #7`)

## Goal and completion

Outcome: the logging contract of whitepaper §16 exists and is verified. Evidence: tests for filter precedence and span/helper field capture; `just lint` + `just test` green; daemon emits JSON on `run`.

## Operating Model

- Daemon logs are machine-consumed (journal, grep, future scrapers); CLI messages are human-consumed.
- The field vocabulary is a contract for #2–#6 — changing it later is a breaking change for log consumers, so it lives in exactly one module.
- §8.4: handler output is inherited; the daemon never blocks on handler I/O.

## Scope and authority

- Agent-delegated: module layout, helper signatures, visitor-based test harness.
- Locked: the four decisions in `spec.md`; no new dependencies; no status op.

## Verification

- Unit: `build_filter` precedence (env > config, invalid env falls back with warning, invalid config errors); capturing-layer test asserting `dispatch_span` fields and one helper's fields appear on nested events.
- `agentd run` smoke: JSON line on stdout.

## Roadmap

1. `src/logging.rs`: `init`, `build_filter`, `events`, `dispatch_span`.
2. Wire into `main.rs` (`Command::Run`); add `pub mod logging` to lib.
3. Tests; `just lint` + `just test`.
4. PR `Closes #7`; re-scope issues #7/#6 as part of it.

## Current state / handoff

- Implemented; tests green; PR merged. See `tasks.md`.
