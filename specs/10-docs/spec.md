# Spec 010 — Documentation (Agent-first)

## Goal

The docs surface for an Agent Native infra whose primary users are Agents:
precise, copy-paste, machine-usable contracts — plus the human ops layer.

## Delivered

- `docs/agent-guide.md` — for Agents: 3-step quickstart, naming identity
  table, full handler contract (stdin/env/exit/no-retry/no-timeout,
  duplicate window + idempotency), concurrency semantics, register/update
  + self-evolution recipe, wire protocol with exact JSON, sending guide,
  debugging (status output, §16 fields), "what agentd will never do".
- `docs/envelope.schema.json` — JSON Schema (2020-12) for Envelope v1;
  pattern mirrors the agent_id grammar (validated consistent).
- `docs/ops-guide.md` — for humans: install, relay + creds policy (0600
  warning, account = injection boundary, per-agent creds), full config
  reference, systemd unit, troubleshooting table, deliberate trade-off
  notes (fsync-per-event, payload copies, env inheritance), security
  summary.
- README quickstart (real usage) + guides index; AGENTS.md routing row.
- Code: `status` carries `daemon_version` (agent feature detection);
  agentdctl prints it; tests updated.
- CHANGELOG summarized for the v0.1 release train.

## Acceptance

- [x] All three docs exist; schema parses and pattern ≡ grammar
- [x] 102/102 + 22/22 e2e green; lint clean
