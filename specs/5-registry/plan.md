# Plan 005 — Agent registry

- **Status:** Approved (design agreed in discussion 2026-08-20; includes dot-separated ids per ADR-0004)
- **Issue:** #5 (`Closes #5`)

## Goal and completion

Outcome: the registry exists with correct persistence, validation, and reload semantics. Evidence: 34/34 tests; `just lint` + `just test` green.

## Locked decisions

1. Two-tier validation (structural hard-error, liveness warning).
2. Persist-then-mutate; failure = no state change.
3. `reload()` returns `Vec<Change>` for #2/#3.
4. Filenames `{agent_id}.toml`, content-level uniqueness; ADR-0004.
5. `dirs` dependency.

## Delegated

File/module shape, helper signatures, test specifics.

## Verification

Unit: validation matrix, duplicate id across files, filename≠id rejection, reload diff across add/remove/update/disable, persist→reload roundtrip, update/unregister flow. `just lint` + `just test`; `dirs` resolves the default agents dir.

## Roadmap

1. `src/registry.rs` + `pub mod registry`.
2. `AgentConfig` validation, persist/load, reload diff.
3. Dot-id sweep across agent_id.rs / whitepaper / examples + ADR-0004.
4. Tests; lint+test green.
5. PR `Closes #5`.
