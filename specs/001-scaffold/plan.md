# Plan 001 — Repository scaffold

- **Status:** Done (initial commit on local `main`; remaining items are GitHub-go gated)
- **Issue:** to be filed as #1 when the GitHub repository is created

## Goal and completion

Outcome: the repo skeleton exists, verifies locally, and encodes the agreed workflow. Evidence: `just lint` and `just test` green; acceptance list in `spec.md` satisfied. Blocked stop: no Rust toolchain or no network to fetch crates — then commit the scaffold unverified and let CI verify.

## Locked decisions (Human)

- Name `agent-daemon`, binaries `agentd`/`agentdctl`, license Apache-2.0 (ADR-0002).
- Workflow: one issue per work item, one PR per issue, concurrent worktrees; `specs/<N>-<slug>/` context folders; `docs/adr/`; Conventional Commits + squash merges; protected `main`; publishing only on explicit go.
- Stack: Rust 2024 stable; deps as listed in `Cargo.toml`; `async-nats` + `rusqlite` deferred to their issues.
- Language: English throughout the repo; Chinese whitepaper kept as reference.
- Tooling: just, nextest, llvm-cov (diagnostic), cargo-deny, git-cliff, Dependabot, cargo-zigbuild release matrix (ADR-0003).

## Delegated mechanics

File-by-file layout, exact CI YAML, module skeletons, test cases — all agent-discretionary within the locked decisions.

## Roadmap

1. Rename dir `agentd` → `agent-daemon` (old path symlinked for the session).
2. Whitepaper: apply v0.1 amendments to Chinese copy → keep as `.zh.md`; write English SoT with amendments folded in.
3. Root files: AGENTS.md, README, LICENSE, Cargo.toml, rust-toolchain, justfile, .gitignore, cliff.toml, deny.toml, CHANGELOG.
4. `.github`: ci.yml (+ commented E2E job skeleton), release.yml, dependabot.yml, issue/PR templates.
5. ADRs 0001–0003; specs/001 (this folder).
6. `src/` skeleton + `tests/cli.rs`.
7. Verify locally; fix until green.
8. Single initial commit on `main` (local only).

## Verification

`cargo build --all-targets`; `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`; `cargo nextest run` (fallback `cargo test`); structural checks on the whitepaper (section count, amendment markers).

## Current state / handoff

- Steps 1–6 complete; see `tasks.md`.
- Known deviation: GitHub repo URL in `Cargo.toml` assumes `Clouder0/agent-daemon` — verify when the repository is created.
