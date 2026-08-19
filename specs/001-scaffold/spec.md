# Spec 001 — Repository scaffold

## Goal

Bootstrap the `agent-daemon` repository: structure, operating rules, CI, release pipeline, specification docs, and a compiling Rust skeleton — everything a work item needs so any fresh agent session can start from a clean clone with full context.

## In scope

- Repo layout per AGENTS.md: `docs/` (whitepaper + ADRs), `specs/<N>-<slug>/`, `.github/`, `.worktrees/` (gitignored), `src/`, `tests/`.
- `docs/whitepaper-v0.md` — English source of truth with v0.1 amendments folded in; Chinese original kept as `docs/whitepaper-v0.zh.md` reference.
- ADRs 0001 (in-flight redelivery dedup), 0002 (project naming), 0003 (build/testing/release).
- Rust 2024 skeleton: `agentd` + `agentdctl` binaries; `agent_id` (grammar + subject encoding), `event` (envelope v0), `config` (daemon TOML), `error` (taxonomy) modules with unit tests; CLI smoke tests.
- CI: lint (fmt + clippy `-D warnings`), test (nextest), coverage (llvm-cov), cargo-deny; release workflow (cargo-zigbuild matrix, tarballs + SHA256SUMS).
- `just` task runner mirroring CI; Dependabot; git-cliff config; Apache-2.0 LICENSE.

## Out of scope

- Any relay / dispatcher / registry / dedup-store / control-socket implementation (separate issues; see whitepaper §17.3).
- `async-nats` and `rusqlite` dependencies (land with the issues that first use them).
- Creating the GitHub repository, pushing, or releases (explicit Human go required).

## Acceptance criteria

1. `cargo build --all-targets`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo nextest run` all pass locally.
2. Every file listed above exists and is committed on local `main`.
3. The English whitepaper contains all 25 sections and the v0.1 amendments marked `(v0.1)`.
4. A fresh agent session can orient from AGENTS.md alone (routing table → whitepaper / ADRs / specs / commands).
