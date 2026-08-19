# ADR-0003: Build, testing, and release strategy

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

The project builds in public and wants professional, layered verification and multi-platform release artifacts, without process theater. v0 is Linux-only by support policy, but statically linked musl binaries make deployment trivial, and keeping the code Unix-portable preserves future options.

## Decision

**Testing (whitepaper §21 is the matrix):**

- Three layers: unit tests (parsers, registry, dedup, concurrency gate) → integration tests against fake handlers and a real Unix control socket, no NATS required → E2E tests against a real `nats-server` with JetStream (§21.2 verbatim, including offline delivery, reconnect, ack-loss, crash window, in-flight redelivery).
- The relay boundary gets a small trait so the dispatcher is testable without Docker.
- Tooling: `cargo-nextest` (runner), `proptest` (agent_id grammar, envelope validation), `cargo-llvm-cov` (coverage as diagnostic, never a gate).
- CI on every PR: fmt + clippy `-D warnings` + unit tests; main adds coverage and `cargo-deny` (advisories + licenses); Dependabot for cargo and actions.

**Release:**

- `cargo-zigbuild` (zig as cross `cc`, which also solves musl + bundled SQLite later).
- Matrix: `x86_64`/`aarch64` × musl (primary, static) / gnu. macOS artifacts are best-effort and unsupported in v0.
- Tag `v*` → CI builds the matrix → tarballs + `SHA256SUMS` → GitHub Release with generated notes.
- `CHANGELOG.md` maintained by `git-cliff` from Conventional Commits; `just` mirrors CI commands locally so agent verification equals CI verification.
- sccache (`RUSTC_WRAPPER=sccache`) recommended locally because per-issue worktrees each own a `target/` — no build-lock contention between concurrent agents, at the cost of a few GB disk.

**Deferred:** criterion benchmarks (v0 perf targets are modest — an I/O-bound personal daemon; profile on evidence), `cargo-dist` (adopt only if the manual pipeline becomes a burden), MSRV policy (none before 1.0).

## Consequences

- PR verification is mechanical and identical locally and in CI — agents cannot self-certify, the gate can.
- musl artifacts cover virtually all Linux deployment targets with zero dynamic-linking friction.
- The scaffold carries a commented-out CI job skeleton for the JetStream E2E suite so wiring it later is trivial.
