# Plan 003 — Dispatcher

- **Status:** Approved (5 decisions ratified in discussion 2026-08-20)
- **Issue:** #3 (`Closes #3`)

## Verification

Integration tests (`tests/dispatcher.rs`) with fake `Acker` and real shell-script handlers cover every acceptance bullet; `just lint` + `just test` green.

## Roadmap

1. `dispatcher.rs`: types + traits + dispatch sequence + apply_changes.
2. tokio dev-dep; tests.
3. PR.

## Current state / handoff

- Implemented; see tasks.md.
