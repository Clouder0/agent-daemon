# AGENTS.md — agent-daemon

`agent-daemon` (binary: `agentd`) is a per-machine daemon for Agent Native Domains: it receives events addressed to locally registered Agents from a self-hosted NATS JetStream relay and turns each event into exactly one local executable invocation — the Handler receives the original event JSON on stdin. It is mechanism only: no agent loop, no context, no retries, no policy. An `agent_id` (dot-separated, e.g. `coding_main`) is a routing name decoupled from process liveness.

This file is a routing table, not a knowledge base — every line here is paid for on every turn. Point, don't inline.

| Context | Home |
|---|---|
| Semantics (source of truth) | `docs/whitepaper-v0.md` — if code and whitepaper disagree, the whitepaper wins until amended by PR |
| User-facing docs | `docs/agent-guide.md` (for Agents), `docs/ops-guide.md` (for humans), `docs/envelope.schema.json` |
| Permanent decisions and rationale | `docs/adr/NNN-*.md` |
| Per-issue working context | `specs/<N>-<slug>/` (spec / plan / tasks) |
| Original Chinese whitepaper | `docs/whitepaper-v0.zh.md` (reference copy, not SoT) |

## Hard scope guardrails

The daemon core must never gain: LLM clients, prompt templates, context builders, agent loops, tool registries, memory, planners, subagents, workflow engines, handler retries, handler timeouts, sender authentication, a local inbox/outbox, or anything listed in whitepaper §23 (Explicit non-goals). If a task appears to require one of these, STOP and raise it with the Human instead of implementing.

## Conventions

- **Language:** English everywhere in this repo — docs, code comments, commit messages, issues, PRs.
- Binaries: `agentd`, `agentdctl`. Crate: `agent-daemon`. Supported platform for v0: Linux.
- Rust 2024 edition, stable toolchain (pinned via `rust-toolchain.toml`). No MSRV promise before 1.0.
- Keep the dependency list minimal; every addition needs a reason in its PR.
- Errors are typed (`thiserror`); no stringly-typed failures.
- Structured logging via `tracing`; log fields per whitepaper §16.
- Tests: unit tests in-module; integration tests in `tests/` against fakes; E2E tests against a real `nats-server` where the whitepaper demands it (§21.2). Never weaken a §21 test to make CI pass.
- License: Apache-2.0. Never commit secrets — `.creds` files are always gitignored.

## Commands (local == CI)

    just build       cargo build --all-targets
    just lint        fmt --check + clippy -D warnings   (must pass before every PR)
    just test        unit tests via cargo-nextest
    just fmt         apply formatting
    just deny        cargo-deny advisories + licenses
    just coverage    cargo-llvm-cov (diagnostic, not a gate)

`sccache` is recommended (`RUSTC_WRAPPER=sccache`) because build dirs are per-worktree.

## Workflow (GitHub-centric)

1. **One issue per unit of work; one PR closes one issue** (`Closes #N`). Multiple issues/worktrees may proceed concurrently.
2. Branch + worktree per issue:
       git worktree add .worktrees/<N>-<slug> -b <N>-<slug>
   The main checkout stays clean on `main`.
3. Non-trivial work: write `specs/<N>-<slug>/spec.md` + `plan.md` (+ `tasks.md`) **before** implementation and get plan approval. Trivial fixes may go straight to a PR.
4. Plans follow the status lifecycle `Draft → Approved → Implementing → Done`. The plan's status section is the session handoff surface — keep it current at meaningful checkpoints so a fresh session (or a post-compaction one) can resume from issue + plan + diff alone.
5. PR requirements: CI green (lint + test), Human review, squash-merge. Conventional Commits (`feat:`, `fix:`, `docs:`, `chore:`, `test:`, `refactor:`); the commit body explains *why*, not what.
6. Decisions that outlive the work item → new `docs/adr/NNN-slug.md`, written with the PR that implements them. Task-scoped decisions stay in the plan. Semantics land in the whitepaper. One fact, one home.
7. Never push to `main` directly; never force-push `main`. Publishing anything (repo creation, pushes, releases) requires the Human's explicit go.
