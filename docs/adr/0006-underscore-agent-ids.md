# ADR-0006: Underscore-separated agent ids (supersedes ADR-0004's separator)

- **Status:** Accepted
- **Date:** 2026-08-20
- **Whitepaper sections:** §2.3, §5.2, §5.3 (amended)
- **Supersedes:** ADR-0004's choice of `.` as the separator (ADR-0004's
  injectivity analysis stands; its conclusion is improved here)

## Context

ADR-0004 moved agent ids from `/`-separated to `.`-separated so that
id == subject == filename. The one mapping it could not make an identity was
the JetStream **consumer name**: NATS durable names cannot contain `.`,
forcing a hash (`agent-<sha256[0..8]>`) — safe (32 bits, collision-checked
at bind) but opaque, and it deletes the readability of the last remaining
derived name.

Reviewing this for #2 exposed a stronger design: nothing requires the id's
internal structure to be visible to NATS. If the id occupies a **single
subject token**, `_` (legal in NATS tokens, consumer names, and filenames)
can be the separator, and *every* derived name becomes an identity:

| mapping | ADR-0004 (`.`) | this ADR (`_`) |
|---|---|---|
| id → filter subject | identity (`agent.events.coding.main`) | identity (`agent.events.coding_main`) |
| id → consumer name | **hash** | identity (`agent-coding_main`) |
| id → config filename | identity (`coding.main.toml`) | identity (`coding_main.toml`) |

What is given up: NATS-level wildcard hierarchy over agent namespaces
(`agent.events.coding.>`). No current or planned design uses it — v0 assigns
exactly one consumer per agent (§5.2/§5.3), §23 rules out the features that
might have wanted it, and any future namespace-wide service can define its
own subject space. Hypothetical flexibility does not pay rent.

## Decision

Grammar:

```text
agent_id := token ("_" token)*
token    := [a-z0-9][a-z0-9-]{0,62}
```

`_` is banned inside tokens (the injectivity requirement ADR-0004
identified); `.` is banned entirely.

- Filter subject: `agent.events.<id>` — the id is one NATS token.
- Durable consumer name: `agent-<id>` verbatim (the prefix groups agentd's
  consumers in server listings). The hash is deleted before it was written.
- Config filename: `<id>.toml` (unchanged mechanics).
- One canonical name across the Domain survives: envelope `agent_id`,
  subject, consumer listing, filename, and CLI all use `coding_main`.

## Consequences

- Whitepaper §2.3/§5.2/§5.3 and all examples amended; code/tests swept
  (pre-release, zero consumers — the last cheap moment, same as ADR-0004).
- The dispatcher's span consumer field (`agent-<id>`) now names the real
  consumer; the #2-era placeholder is retired by construction.
- Consumer listings on the server are human-readable without a lookup table.
