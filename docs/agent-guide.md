# Agent Guide

**This guide is written for Agents** — you, an Agent that wants to receive
events, register handlers, update yourself, or debug your own event flow.
Precision over prose: every contract is stated exactly once, with a runnable
example.

- Spec of record: [`whitepaper-v0.md`](whitepaper-v0.md) (code never
  overrules it).
- Human operators: see the [ops guide](ops-guide.md).

---

## TL;DR — receive your first event in 3 steps

```bash
# 1. Write a handler: any executable reading the event JSON from stdin.
cat > ~/agents/my-agent/on-event <<'EOF'
#!/bin/sh
cat > /tmp/last-event.json
EOF
chmod +x ~/agents/my-agent/on-event

# 2. Register (the relay must already be initialized — see ops guide).
agentdctl register --id my_agent --handler ~/agents/my-agent/on-event

# 3. Someone publishes to your subject; your handler runs.
```

Your subject is `agent.events.my_agent`. A minimal valid event:

```json
{
  "version": 1,
  "event_id": "0192f0c5-1a2b-7c3d-8e4f-5a6b7c8d9e0f",
  "agent_id": "my_agent",
  "type": "im.message",
  "created_at": "2026-08-20T12:00:00Z",
  "payload": {}
}
```

---

## Naming (memorize this once)

`agent_id` grammar (`_`-separated):

```text
agent_id := token ("_" token)*
token    := [a-z0-9][a-z0-9-]{0,62}
```

One name, everywhere — all mappings are identity:

| Thing | Value for `coding_main` |
|---|---|
| Filter subject | `agent.events.coding_main` |
| Durable consumer | `agent-coding_main` |
| Config file | `agents.d/coding_main.toml` |
| Envelope `agent_id` | `coding_main` |

Rules: lowercase `[a-z0-9]` first, then `[a-z0-9-]` (≤63 chars/token);
`_` separates; `.` and `/` are illegal inside ids.

---

## Handler contract

When an event for you arrives, agentd execs your handler **exactly once per
dispatch**:

| Input | Guarantee |
|---|---|
| stdin | The **original event bytes**, unmodified, UTF-8 JSON, ≤ `max_event_bytes` (default 256 KiB). Never re-serialized. |
| Environment | `AGENTD_AGENT_ID`, `AGENTD_EVENT_ID`, `AGENTD_EVENT_TYPE`, `AGENTD_STREAM_SEQUENCE`, `AGENTD_CONSUMER_SEQUENCE`, `AGENTD_DELIVERY_COUNT` (§8.3; stdin stays authoritative). |
| Working dir | Your registered `working_directory` (if set). |
| stdout/stderr | Inherited — write logs freely; they land in the daemon journal. |
| Identity | Same Unix user as agentd. You inherit agentd's environment (do not keep secrets there). |

Exit semantics — **read this once, it is the whole model**:

- agentd **never retries** on your exit code. `exit 0`, `exit 1`, signal
  death — all identical to agentd: the dispatch is finished, the event is
  acked.
- agentd **never kills** you. There is no timeout. Run for hours if your
  job needs it (you occupy one concurrency slot; with the default serial
  mode, later events wait in the relay).
- If you exceed the slow-handler threshold (default 1h), a WARNING is
  logged. Nothing else happens.
- If your executable is missing or not executable, the event is
  terminally dropped (logged) — fix the registration and re-publish.

Retry policy is yours: loop, sleep, and retry inside the handler, or write
to your own queue and exit.

### Concurrency

- `max_concurrency = 1` (default): events run strictly one at a time, in
  delivery order.
- `max_concurrency = N`: up to N handlers at once; completion order is not
  guaranteed; finer-grained locking (per repo, per conversation) is your
  job inside the handler.

### Duplicates — the one window you must handle

Delivery is **best-effort effectively-once**. In rare windows (daemon crash
between your side effects and the completion record; relay outage during a
redelivery) the same event can reach your handler twice. Make side effects
idempotent using `AGENTD_EVENT_ID` (or `event_id` from stdin) — e.g. unique
keys, upserts, idempotency headers. A duplicate copy arriving **while you
are still running** is dropped without a second spawn; duplicates after
completion are suppressed by the dedup store (per `(agent_id, event_id)`).

### Recommended shape: a handoff program

```sh
#!/bin/sh
# Read the event, hand it to your runtime, exit fast.
EVENT=$(cat)
ensure_my_runtime_running
deliver "$EVENT"   # or: enqueue locally; or: start the runtime yourself
```

Hours-long work belongs in your runtime/background jobs, not the handler
process (it works, but it holds a slot and blocks serial mode).

---

## Registering and updating yourself

Self-management is a **feature**, not a privilege (§13). Operations:

```bash
agentdctl register --id my_agent --handler /abs/on-event [--max-concurrency 1] [--cwd DIR]
agentdctl update   my_agent [--handler /abs/on-event-v2] [--max-concurrency 4] [--cwd DIR] [--enable|--disable]
agentdctl unregister my_agent
agentdctl list
agentdctl reload      # re-read agents.d from disk
agentdctl status      # daemon version, nats state, per-agent backlog
```

- `update` takes effect immediately: the next event uses the new handler;
  a running handler is never interrupted (§7.4).
- `--disable` stops consumption; in-flight work drains; `--enable` resumes.

### Self-evolution recipe (§11.6)

```text
1. deploy new runtime + write new handler
2. test it standalone (feed it a sample event on stdin)
3. agentdctl update my_agent --handler .../on-event-v2
4. next events flow to the new handler
5. old process exits whenever it is done
```

agentd never needs to restart, and never knows a generation handoff
happened.

### Wire protocol (speak the socket directly)

`agentdctl` is a convenience; the control socket is a stable line-JSON
protocol (§18) at `$XDG_RUNTIME_DIR/agentd/control.sock` (mode 0600,
same-user). One JSON request per line, one JSON response per line:

```json
{"op":"register","agent":{"agent_id":"my_agent","handler":"/abs/on-event","max_concurrency":1,"working_directory":"/abs/cwd","enabled":true}}
```
```json
{"ok":true}
```

All ops: `register` (agent object as above; fields optional:
`max_concurrency` default 1, `working_directory`, `enabled` default true),
`update` (same object), `unregister` (`{"op":"unregister","agent_id":"..."}`),
`list` (response carries `agents`), `reload`, `status` (response carries
`status` with `daemon_version`, `nats_connected`, per-agent
`in_flight`/`num_pending`/`num_ack_pending`). Errors:
`{"ok":false,"error":"..."}`. Unknown envelope fields are ignored.

---

## Sending events (to yourself or other agents)

Publish the envelope JSON to `agent.events.<agent_id>`. Machine-usable
schema: [`envelope.schema.json`](envelope.schema.json). Required fields:
`version` (=1), `event_id` (globally unique, e.g. UUIDv7/ULID), `agent_id`,
`type`, `created_at` (RFC 3339), `payload` (any JSON). `metadata` and
unknown fields pass through untouched. Senders are unverified (§3.4) —
anything you need to trust in `metadata` (signatures, sender claims), you
must check yourself in the handler.

---

## Debugging your flow

`agentdctl status`:

```text
daemon: v0.1.0
nats: connected
AGENT                    STATE    CONC  INFLIGHT  PENDING  ACKPEND
my_agent                 enabled  1     0         0        0
```

- `PENDING` growing = events waiting (you are serial and slow, or the
  daemon stopped pulling — check `STATE` and the daemon log).
- `ACKPEND` growing = dispatched but not acked yet (handlers running).

Structured log fields you can rely on (§16), on every dispatch line via the
span chain: `agent_id`, `event_id`, `consumer`, `stream_sequence`,
`handler_path`, `handler_pid`, `duration_ms`, `exit_status`. Key events:
`event received`, `dedup hit`, `in-flight duplicate dropped`,
`handler spawned`, `handler exited`, `ack succeeded/failed`,
`invalid event`, `nats connected/disconnected`.

## What agentd will never do

No retries on your behalf, no timeouts, no health checks, no wake-up logic,
no sender verification, no payload inspection (beyond envelope validation),
no storage of your events. It is the last mile of wiring — everything
agent-specific is yours (§23).
