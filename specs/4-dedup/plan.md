# Plan 004 — Dedup store

- **Status:** Approved (consensus reached 2026-08-20: Option B composite key; fail-open policy)
- **Issue:** #4 (`Closes #4`)

## Goal and completion

Outcome: the completed-event dedup primitive exists and is verified. Evidence: unit tests incl. the ADR-0005 regression (same event_id, different agents); `just lint` + `just test` green.

## Verification

In-memory store tests: insert/hit, idempotent re-mark, TTL purge boundary, composite-key isolation, concurrent hammer, file-backed open creating the schema.

## Roadmap

1. ADR-0005 + whitepaper §10.2/§10.3 amendment (EN SoT + zh reference).
2. `rusqlite` (bundled) dep; `resolved_dedup_path()`; `AgentdError::DedupStore`.
3. `src/dedup.rs`: open (pragmas + schema + startup purge), is/mark/purge.
4. Tests; lint+test green.
5. PR `Closes #4`.

## Current state / handoff

- Implemented; see `tasks.md`.
