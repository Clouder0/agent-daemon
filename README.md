# agent-daemon

**An edge-side event dispatch daemon for Agent Native Domains.** Binary: `agentd`.

In an Agent Native Domain, agents are replaceable software generations; what persists is the world outside any harness — messages, repositories, artifacts, humans. `agentd` is one stable piece of that world: it lets a machine's agents stay reachable even while they are not running.

```text
External Services (IM / Email / GitHub / CI / …)
        │  adapters / workers (not agentd)
        ▼
Self-hosted NATS JetStream          ← events persist while machines sleep
        │  durable pull consumers
        ▼
agentd                               ← one event → one local executable invocation
        │  event JSON on stdin
        ▼
Agent-owned Handler                  ← all agent policy: auth, wake, queue, steer, retry
        ▼
Agent Runtime                        ← understands the event, decides what to do
```

`agentd` is deliberately mechanism-only: no LLM clients, no agent loop, no context management, no retries, no sender verification. Everything agent-specific lives in a handler executable each agent registers. An `agent_id` (dot-separated, e.g. `coding_main`) is a routing name decoupled from process liveness — which is what makes agent sleep, migration, and self-replacement (a new generation updating its own handler binding) first-class.

**Status:** pre-alpha, building in public. The v0 specification is [`docs/whitepaper-v0.md`](docs/whitepaper-v0.md).

## Features (v0 target)

- One long-running daemon per machine hosting many agents (ids like `coding_main`)
- Durable offline delivery via NATS JetStream (per-agent durable pull consumers)
- Dispatch = execute the registered handler with the event JSON on stdin
- Serial per agent by default; configurable concurrency
- No retry on handler exit code; terminal handling of poison events
- Best-effort effectively-once dispatch (completed-event dedup + in-flight redelivery guard)
- Dynamic registration via local control socket and `agentdctl`
- Structured logging; graceful shutdown

Explicit non-goals for v0 are listed in the whitepaper (§23).

## Build

Rust, stable toolchain (Linux for v0):

```bash
just build    # or: cargo build --all-targets
just lint     # fmt --check + clippy -D warnings
just test     # unit tests (cargo-nextest)
```

Release artifacts are produced by CI (`cargo-zigbuild`) for `x86_64`/`aarch64` × musl/gnu on tagged releases.

## Documentation

- [Whitepaper v0 (specification, source of truth)](docs/whitepaper-v0.md)
- [Architecture decision records](docs/adr/)
- [Per-issue specs and plans](specs/)

## Development model

One GitHub issue per unit of work, one PR per issue; per-issue worktrees and context folders under `specs/`. See [AGENTS.md](AGENTS.md) for the full operating model.

## License

Apache-2.0. See [LICENSE](LICENSE).
