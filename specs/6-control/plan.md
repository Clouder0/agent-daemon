# Plan 006 — Control plane

- **Status:** Approved (decisions ratified 2026-08-20; status scope + deferral per user)
- **Issue:** #6 (`Closes #6`)

## Verification

tests/control.rs (7 cases, fake backend) + live docker smoke (9 steps, real
binaries) — both green. 72/72 suite.

## Current state / handoff

Implemented, smoke-passed, self-reviewed (serve concurrency, apply-path
sharing, socket lifecycle, agentdctl borrow/lint cleanup).
