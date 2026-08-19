# Spec 005 — Agent registry

## Goal

The registry the dispatcher resolves `agent_id` against (whitepaper §7), with the `agents.d/*.toml` persistence store and reload-as-diff semantics.

## In scope

- `AgentConfig` (agent_id, handler, max_concurrency, working_directory, enabled), `agents.d/` load/persist, reload, register/update/unregister/set_enabled.
- Two-tier validation: structural = hard error (absolute handler, positive concurrency, absolute cwd, matching filename); liveness = warning (handler missing / not executable).
- Persist-then-mutate via temp-file → fsync → atomic rename; in-memory state unchanged on write failure.
- `Change` diff enum (Added/Updated/Enabled/Disabled/Removed) for #2/#3 to subscribe to.
- **Dot-separated agent ids** (ADR-0004) folded in — the id, subject, and filename are the same dot-form; collision handling removed entirely.
- `dirs` dependency for `$XDG_CONFIG_HOME`.

## Out of scope

- Waiting-for-in-flight-handlers on unregister (#3 dispatcher behavior — #5 emits `Removed`/`Disabled` only); consumer create/bind (#2); control socket (#6).
- No concurrency machinery beyond `std::sync::RwLock` with short critical sections.

## Locked decisions

1. Two-tier validation (structural error vs liveness warning) — a deleted handler is a §8.6 dispatch-time error, not a load failure.
2. Persist-then-mutate; write failure = zero state change.
3. `reload()` returns `Vec<Change>`; consumers subscribe.
4. Filenames = `{agent_id}.toml`, content-level uniqueness; filename must match content id (ADR-0004).

## Acceptance criteria

- [ ] `AgentConfig` validated: absolute handler, `max_concurrency >= 1`, absolute cwd
- [ ] `agents.d/*.toml` loaded; duplicate content `agent_id` and filename≠id rejected
- [ ] Atomic writes (temp → fsync → rename); failure leaves state unchanged
- [ ] `reload()` returns the diff; register/update/unregister/set_enabled persist correctly
- [ ] Unit tests: validation, duplicate detection, reload transitions, roundtrip
