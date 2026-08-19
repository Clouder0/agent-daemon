# ADR-0004: Dot-separated agent ids

- **Status:** Accepted
- **Date:** 2026-08-20
- **Whitepaper sections:** §2.3, §5.2, §7, §8, §20

## Context

The v0 grammar used `/` as the segment separator:

```text
agent_id := segment ("/" segment)*
```

An `agent_id` must simultaneously be: a stable identity; a routing key mapped
to one NATS filter subject; and a filesystem-safe single filename (one config
file per agent). The `/` separator forced two lossy, non-injective transforms:

- `/` → `.` for the NATS subject (`coding/main` → `agent.events.coding.main`);
- `/` → `-` for the config filename (`coding/main` → `coding-main.toml`).

The filename mapping collides: `a/b-c` and `a-b/c` both flatten to
`a-b-c.toml`. The naive form also made the registry's collision error branch
necessary as a normal path rather than a defense.

## Decision

Use `.` as the separator instead:

```text
agent_id := token ("." token)*
token    := [a-z0-9][a-z0-9_-]{0,62}
```

Consequences that make this strictly simpler:

- **identity == subject == filename**: `coding.main` → `agent.events.coding.main`
  (prefix only) and `coding.main.toml` — zero transforms, injective, no
  collisions.
- `.` is already NATS's native subject separator, so the mapping is the
  identity and any future hierarchical scoping (`agent.events.coding.>`,
  per-org wildcards) is *more* natural than with `/`-ids.
- `.` remains excluded inside a token, so the grammar is unambiguous.
- Filenames `coding.main.toml` are still unambiguous against the `.toml`
  suffix and never collide with other valid ids.

Adopted pre-v0.1 with no consumers, so the change is a coordinated doc + code
sweep only, no migration.

## Consequences

- Registry (#5) drops its collision-error branch; filenames are direct
  `{id}.toml`.
- The subject encoding helper collapses to a prefix concatenation.
- Cost: a readability shift (`coding.main` vs `coding/main`) and a doc sweep
  across §2.3/§5.2/§7/§8/§20 and examples.
