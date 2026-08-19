# ADR-0002: Project naming — `agent-daemon`

- **Status:** Accepted
- **Date:** 2026-08-19

## Context

The project was drafted under the name `agentd`. Before creating a public repository, availability checks (2026-08-19) found:

- GitHub: no prominent repo named exactly `agentd`, but the `agent*` namespace is crowded (AgentDock, agentdojo, agentdesk, …), and an established project operates as **agentd** at `github.com/agentd-dev` (an MCP-native agent runtime, v2.2.0).
- crates.io: the crate name **`agentd` is taken** (unrelated capability-execution daemon, 0.1.2); `agentd-core` is taken by the agentd-dev project. `agent-daemon` was free.

Building in public under an ambiguous name is a permanent discoverability and confusion tax.

## Decision

- Project / repository / crate name: **`agent-daemon`**.
- Binaries keep the whitepaper vocabulary: **`agentd`** (daemon) and **`agentdctl`** (control CLI). Crate name ≠ binary name is standard Rust practice.
- Distribution: GitHub Release artifacts + `cargo install --git`; a crates.io publish under `agent-daemon` may happen later if it earns itself.
- Documentation and whitepaper refer to the daemon as `agentd` where it reads naturally; the project identity is `agent-daemon`.

## Consequences

- No collision with the existing `agentd` ecosystem; the crate name is available if publishing becomes worthwhile.
- Two names to keep straight: repo/crate `agent-daemon`, binaries `agentd`/`agentdctl`. Recorded in AGENTS.md and README to prevent drift.
