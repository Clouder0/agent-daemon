# Plan 002 — Relay + run assembly

- **Status:** Approved (6 decisions ratified 2026-08-20; unified-`_` per ADR-0006)
- **Issue:** #2 (`Closes #2`)

## Verification

Unit tests (consumer naming/config), 65/65 suite, plus a live smoke against
`docker run nats:latest -js`: init stream → register agent → publish → handler
receives exact original bytes → consumer durable/visible → SIGTERM graceful.

## Current state / handoff

- Implemented + smoke-passed; see tasks.md.
