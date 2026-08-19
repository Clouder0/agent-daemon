# agent-daemon v0 Whitepaper (binaries: `agentd` / `agentdctl`)

## An edge-side event dispatch daemon for the Agent Native Domain

**Status: Implementation SoT / v0.1 Amended**
**Intended audience: the Coding Agent implementing `agent-daemon`**
**Date: 2026-08-19 (v0.1 amendments: 2026-08-20)**

> The v0.1 amendments incorporate the design-review decisions (see `docs/adr/`); amended passages are marked `(v0.1)`. Highlights: in-flight redelivery dedup (ADR-0001), daemon-level config file, `agentdctl init` owns stream creation, AckWait defaults, content-level uniqueness for `agents.d`, unregister waits for in-flight acks, DeliverPolicy replay edge, Envelope version-upgrade rule, concurrent stdin writes. A Chinese reference copy of the original whitepaper is kept at `docs/whitepaper-v0.zh.md`; this English document is the source of truth.

---

## Abstract

`agent-daemon` (binary: `agentd`) is a lightweight daemon running on one specific machine. It maintains a persistent connection to the message Relay of its Domain, receives events addressed to local Agents from NATS JetStream, finds the locally registered Handler for each Agent via its `agent_id`, and executes the Handler with the event delivered on standard input.

It does not understand Agent Loops, does not manage Context, does not judge whether an Agent is alive, does not start Agents, does not verify business senders, and does not handle Handler retries or failure recovery. All Agent-specific logic lives in the Handler each Agent registers.

The overall boundary can be summarized as:

```text
NATS JetStream
    responsible for making events durably exist,
    and retaining them while agentd is offline

agentd
    responsible for turning one event
    into one local executable invocation

Agent-owned Handler
    responsible for deciding what an event means,
    and how to find, start, or notify the real Agent

Agent Runtime
    responsible for understanding the event,
    building context, and autonomously deciding what to do next
```

The goal of `agentd` is not to become another Agent Harness, but to be a stable, simple, reusable piece of edge-side infrastructure within the Agent Native Domain.

---

# 1. Background: why `agentd` exists

## 1.1 From inside a Harness to Agent Native Infrastructure

Most agents today run inside some Harness.

A Harness typically provides:

* maintaining model sessions;
* constructing Context;
* driving the Agent Loop;
* registering and executing Tools;
* managing Sandboxes;
* presenting the Human Interface;
* persisting some run state.

This shape suits an individual piece of Agent work, but it easily ties an Agent's existence to one running Harness process.

When the Harness process ends, the Agent typically loses, along with it:

* a persistently reachable message endpoint;
* a stable external identity;
* the ability to be called back when asynchronous tasks complete;
* the ability to resume work from a dormant state;
* the ability to migrate between Runtimes or machines.

We prefer a different model:

> An Agent may replace its own Harness, or even start a modified successor Agent, and only then exit.

In this model, one Generation of an Agent is a replaceable software individual. What truly persists is the world outside the Harness:

* messages;
* repositories;
* artifacts;
* compute resources;
* external services;
* Humans;
* other Agents;
* the relationships between them.

`agentd` sits exactly on that boundary.

It lets an Agent on a machine remain reachable by the Domain's external Infrastructure even when the Agent is not currently running.

---

## 1.2 Where `agentd` sits in the Agent World

The Agent World is not a central platform.

It consists of many Personal Domains, Enterprise Domains, public services, and standalone machines. Each Domain can deploy its own:

* IM;
* Email;
* Git;
* CI;
* Sandboxes;
* NATS;
* Agent Hosts;
* other internal Infrastructure.

`agentd` is not a global service, nor a cross-Domain unified Runtime.

It is a **daemon running locally on each machine**:

```text
Personal Domain
├── Self-hosted NATS JetStream
├── Desktop A
│   └── agentd
│       ├── coding.main
│       └── assistant.personal
└── Server B
    └── agentd
        └── research.main
```

One `agentd` can host multiple Agents.

In v0, one `agent_id` is consumed by exactly one `agentd` at a time. An Agent can migrate to another machine, but migration is performed explicitly by the Agent or a Human updating configuration; v0 does not implement automatic leader election or multi-machine leases.

---

# 2. Core definitions

## 2.1 Domain

A Domain is an environment sharing trust, infrastructure, and an administrative boundary.

A Domain may belong to:

* a single user;
* a company;
* a team;
* a household;
* a public service provider.

v0 assumes:

* the Relay and `agentd` belong to the same Domain;
* local Agents trust each other by default;
* multi-tenancy, zero trust, and complex Agent IAM are out of scope.

---

## 2.2 Relay

The Relay is long-lived messaging infrastructure inside the Domain.

v0 uses self-hosted NATS JetStream:

* external Adapters or Workers publish events to JetStream;
* `agentd` receives events through a durable Consumer;
* while `agentd` is offline, events remain stored in JetStream;
* after reconnecting, `agentd` resumes consuming.

Core NATS delivers messages only to currently connected subscribers, whereas JetStream adds persistence, Consumer progress, and replay — so senders and receivers do not need to be online simultaneously.

---

## 2.3 Agent ID

An `agent_id` is a logical Agent name within a Domain.

For example:

```text
coding.main
assistant.personal
research.market
```

It is not:

* a PID;
* a Container ID;
* a hostname;
* some Pi Session ID;
* some LLM Request ID.

It means only:

> this event should be handed to the local Handler registered by this logical Agent.

v0 grammar:

```text
agent_id := token ("." token)*
token    := [a-z0-9][a-z0-9_-]{0,62}
```

`.` is both the id separator and NATS's own subject separator, so the id, its filter subject, and its config filename are the same dot-form (identity, injective — ADR-0004):

```text
coding.main
→ agent.events.coding.main
```

---

## 2.4 Handler

A Handler is a local executable registered by the Agent itself.

It can be:

* a Python script with a shebang;
* a Bash script;
* a Rust/Go binary;
* a Node program;
* any executable that can read a JSON Event from stdin.

`agentd` does not constrain the Handler's language.

The Handler owns all Agent-specific logic, for example:

* verifying sender identity;
* checking message signatures;
* deciding whether a message is trusted;
* deciding whether the Agent is currently running;
* starting Pi, Codex, DSH, or any other Runtime;
* deciding to queue, steer, ignore, or create a new Session;
* implementing its own retries;
* implementing its own file locks;
* forwarding events to other machines;
* handling Agent self-migration;
* writing events into the Agent's own persistent state.

---

## 2.5 Agent Runtime

The Agent Runtime is the program that actually executes the Agent Loop.

For example:

* the Pi Coding Agent;
* Codex;
* DeepSeek Harness;
* a custom Python Agent;
* a next-generation implementation produced by the Agent itself.

`agentd` does not know any specific Runtime.

If Pi needs a special startup procedure, that logic lives in the Pi Agent's Handler, not in the `agentd` core.

---
# 3. Design principles

## 3.1 `agentd` is not an Agent Harness

`agentd` must never contain:

* an LLM client;
* prompt templates;
* a Context builder;
* an Agent Loop;
* a Tool registry;
* Memory;
* a Planner;
* Subagents;
* a Workflow engine.

It only executes local programs.

---

## 3.2 Policy stays in the Handler

`agentd` does not judge:

* who may message an Agent;
* whether a message is worth processing;
* whether an Agent should be woken;
* which Session a message belongs to;
* whether a retry is needed;
* whether some Project must be processed serially;
* whether a new Container should be requested.

These are Policies of a specific Agent.

`agentd` provides Mechanism only.

---

## 3.3 Local Agents trust each other by default

The v0 target is a Personal Domain or a single-user machine.

Therefore:

* local Agents may update `agentd` configuration dynamically;
* no Agent-level authentication;
* no per-Agent capability tokens;
* no local multi-tenant isolation;
* Handlers run as the same Unix user as `agentd` by default.

The local security boundary relies on ordinary file permissions and the Unix user boundary.

If this evolves into a Domain-level, multi-user `agentd`, separate identities, permissions, and Sandboxes can be added later.

---

## 3.4 Relay connections must be authenticated

Although local Agents are trusted by default, `agentd` must not accept events from arbitrary public origins.

The connection between `agentd` and NATS must be authenticated.

v0 recommends:

* one dedicated NATS `.creds` per `agentd` instance;
* one NATS Account per Domain;
* NATS over TLS;
* the credential file readable only by the user running `agentd`.

A NATS `.creds` file contains a User JWT and the NKey seed used to sign server challenges; treat it as a secret, like a password.

This authentication answers only:

> the current connection does belong to a legitimate `agentd` of this Domain.

It does not answer whether the business sender claimed inside an Event is genuine.

Sender authentication is left to the Handler.

---

## 3.5 No strict exactly-once

JetStream provides at-least-once delivery: unacknowledged messages may be redelivered.

`agentd` should mask normal network redeliveries so that downstream usually sees an Event exactly once.

But v0 does not introduce:

* two-phase commit;
* distributed transactions;
* Handler recovery probes;
* a local durable inbox;
* a full exactly-once protocol.

In rare cases such as:

```text
Handler has already produced side effects
→ agentd has not yet recorded completion
→ the machine suddenly loses power
```

the same Event may invoke the Handler again.

Handlers must be aware of this and use `event_id` for idempotency where it is cheap to do so.

---

# 4. Overall architecture

```text
External Services
IM / Email / GitHub / CI / Custom API
                    │
                    ▼
          Adapter / Worker Layer
   validate external protocol, convert into Events
                    │
                    ▼
       Self-hosted NATS JetStream
           Durable Event Relay
                    │
              Pull Consumer
                    │
                    ▼
                 agentd
        target agent_id → executable
                    │
                    ▼
          Agent-owned Handler
                    │
        check / wake / queue / steer
                    │
                    ▼
             Agent Runtime
```

Note that:

* Adapters / Workers are not part of `agentd`;
* the NATS Server is not part of `agentd`;
* Handlers are not part of the `agentd` core;
* Agent Runtimes are not part of `agentd`;
* the Human Interface is not part of `agentd`.

`agentd` covers only the last hop of dispatch between the Relay and local Handlers.

---

# 5. NATS JetStream design

## 5.1 Stream

v0 uses one shared Stream:

```text
Stream Name:
    AGENT_EVENTS

Subjects:
    agent.events.>

Storage:
    File

Retention:
    LimitsPolicy
```

Limits retention lets messages survive after Consumer acks until `MaxAge`, `MaxBytes`, or `MaxMsgs` limits are reached, which helps debugging and manual replay; acks advance the Consumer position rather than deleting messages from the Stream directly.

Recommended defaults:

```text
MaxAge:
    7 days

Replicas:
    1 for personal/self-host v0

MaxBytes:
    configurable

MaxMsgSize:
    256 KiB for v0
```

(v0.1) The Stream is created or reconciled explicitly by `agentdctl init` (an operator-time, one-shot action using operator-grade credentials); the credentials used by a running `agentd` need only Consumer-related permissions, never stream-creation permission.

Large files, images, or artifacts must not be placed directly into the Event payload; pass references through an external Object Store, Git, or a file service instead.

---

## 5.2 Subject

Each Agent maps to one subject:

```text
agent.events.<encoded-agent-id>
```

Encoding rule (identity, ADR-0004):

```text
coding.main
→ agent.events.coding.main

assistant.personal
→ agent.events.assistant.personal
```

`agentd` does not subscribe to a global `agent.events.>` and filter by itself.

It creates or binds one dedicated Consumer per registered Agent.

---

## 5.3 Consumer

Each `agent_id` maps to one Durable Pull Consumer.

For example:

```text
Agent ID:
    coding.main

Filter Subject:
    agent.events.coding.main

Durable Consumer:
    agent-<stable-hash-of-agent-id>
```

Consumer configuration:

```text
AckPolicy:
    Explicit

DeliverPolicy:
    All

MaxAckPending:
    max_concurrency
```

A Pull Consumer lets the client control when to fetch and how many messages at a time; a Durable Consumer retains consumption progress after the client disconnects.

v0 constraint:

> one Durable Consumer is owned by exactly one `agentd` at a time.

If two `agentd` instances consume the same Agent's Consumer simultaneously, JetStream may distribute messages across both clients. v0 treats this as a configuration error; no leases, ownership election, or automatic preemption.

(v0.1) Note the case where a Consumer is deleted server-side and later recreated: `DeliverPolicy: All` will replay the full retained history of the Stream (up to `MaxAge`). This is known semantics; use `--deliver-new` at registration time when a fresh start is intended. The normal path is unaffected — while a Durable Consumer exists, consumption always resumes from its stored progress.

---

## 5.4 Acks and long-running Handlers

`agentd` sends the final Ack only after the Handler process has exited.

If a Handler runs longer than the Consumer's `AckWait`, JetStream considers the Consumer stalled and redelivers. JetStream supports `in-progress` acks, which reset the `AckWait` timer and prevent long tasks from being misjudged as failures.

(v0.1) Defaults: `AckWait = 5m`, `in-progress` every `90s`; both configurable.

Therefore v0 SHOULD:

* send `in-progress` periodically while the Handler process is alive;
* send the final Ack after the Handler exits;
* never choose Ack, Nak, or retry based on the Handler's exit code.

An `in-progress` only maintains the current delivery lease; it does not mean `agentd` is responsible for Handler retries.

---

## 5.5 NATS credentials

Each machine's `agentd` uses its own credential.

Recommendations:

```text
Domain:
    one NATS Account

Machine:
    one NATS User / .creds file

Credential:
    stored locally with mode 0600
```

v0 may grant `agentd` fairly broad in-Domain permissions:

* read `agent.events.>`;
* create or update the corresponding Consumers;
* publish status messages that may be needed in the future.

NATS permissions are subject-based publish/subscribe allowlists; per-machine restrictions on which Agent subjects may be consumed can come later.

v0 does not require such fine-grained restrictions.

---

# 6. Event Envelope v0

External Adapters or Workers should convert arbitrary external input into a minimal JSON Envelope.

Example:

```json
{
  "version": 1,
  "event_id": "01J6ZP8R5EF4Y42KABCD123456",
  "agent_id": "coding.main",
  "type": "im.message",
  "created_at": "2026-08-19T12:00:00Z",
  "payload": {
    "text": "Please continue checking the test results"
  },
  "metadata": {
    "source": "matrix",
    "room_id": "!example:domain.test",
    "sender": "@alice:domain.test"
  }
}
```

## 6.1 Required fields

### `version`

The Envelope version.

v0 accepts only:

```json
"version": 1
```

(v0.1) An unknown `version` (e.g. `2`) is treated as a Terminal Event — logged, then acked, never retried; from the sender's perspective the event is dropped. Version rule: backward-compatible additions keep `version: 1`; only incompatible changes bump the version, and a bump is a coordinated Domain upgrade (upgrade all `agentd` instances before senders may emit the new version).

### `event_id`

A globally unique, stable Event identifier.

Recommended:

* UUIDv7;
* ULID;
* or any other globally unique string.

`event_id` is used for best-effort deduplication.

It does not encode ordering.

### `agent_id`

The target Agent.

Must match an Agent ID in the local registry.

### `type`

An event type hint.

For example:

```text
im.message
email.received
ci.completed
github.review
timer.fired
custom
```

`agentd` does not interpret `type`.

### `created_at`

The external event's creation time, RFC 3339 UTC.

### `payload`

Any JSON value.

`agentd` does not interpret the payload.

---

## 6.2 Optional fields

### `metadata`

Any JSON object.

May contain:

* sender claims;
* signatures;
* reply targets;
* conversation IDs;
* artifact references;
* trace context;
* external service metadata.

All of these are left for the Handler to interpret.

---

## 6.3 Fields `agentd` is allowed to interpret

`agentd` interprets only:

```text
version
event_id
agent_id
```

All other fields must be passed to the Handler unchanged.

Unknown fields must not cause an Event to be rejected.

This allows the Envelope to evolve later without upgrading every `agentd` in lockstep.

---
# 7. Local Agent registration

(v0.1) The daemon's own configuration persists at `$XDG_CONFIG_HOME/agentd/agentd.toml` and contains: NATS URL, credential path, Stream name, dedup store path and TTL, control socket path, log level, and similar daemon-level settings. Agent registrations live in `agents.d/` (see 7.4).

## 7.1 Registration model

One machine's `agentd` can register multiple Agents:

```text
agentd
├── coding.main
├── assistant.personal
└── research.market
```

Each Agent registers:

* `agent_id`;
* Handler path;
* maximum concurrency;
* optional working directory;
* enabled state.

Example configuration:

```toml
agent_id = "coding.main"
handler = "/home/clouder/agents/coding-main/on-event"
max_concurrency = 1
working_directory = "/home/clouder/projects/main"
enabled = true
```

---

## 7.2 Handler path

The Handler path must:

* be absolute;
* come from local configuration, never from an Event;
* point to a local executable;
* never be assembled through shell string interpolation.

Correct:

```text
execve("/home/clouder/agents/coding-main/on-event", ...)
```

Wrong:

```text
sh -c "<event supplied command>"
```

Python Handlers should carry a shebang:

```python
#!/usr/bin/env python3
```

and have the executable bit set.

---

## 7.3 Dynamic configuration

Agents must be able to register, update, and unregister themselves dynamically.

v0 provides a local control socket:

```text
$XDG_RUNTIME_DIR/agentd/control.sock
```

The socket defaults to:

```text
mode 0600
```

No additional authentication. Local Agents under the same Unix user are trusted by default.

Minimal operations:

```text
register
update
unregister
list
reload
```

Recommended CLI:

```bash
agentdctl register \
  --id coding.main \
  --handler /home/clouder/agents/coding-main/on-event \
  --max-concurrency 1 \
  --cwd /home/clouder/projects/main

agentdctl update coding.main \
  --handler /home/clouder/agents/coding-main-v2/on-event

agentdctl unregister coding.main

agentdctl list

agentdctl reload
```

---

## 7.4 Persisted configuration

Local registrations persist under:

```text
$XDG_CONFIG_HOME/agentd/agents.d/
```

One TOML file per Agent:

```text
agents.d/
├── coding-main.toml
├── assistant-personal.toml
└── research-market.toml
```

(v0.1) The `/`→`-` filename mapping is not injective (`a/b-c` and `a-b/c` both produce `a-b-c.toml`), so uniqueness is enforced on the `agent_id` inside the file content: loading fails on duplicate `agent_id`s; filenames are display convention only.

Updates must use:

```text
write temporary file
→ fsync if practical
→ atomic rename
```

After `agentd` reloads:

* new Agents: create or bind a Consumer and start consuming;
* updated Agents: future Events use the new Handler;
* disabled Agents: stop pulling new Events;
* removed Agents: stop consuming, but never delete original messages in the Stream;
* running Handlers are not terminated by a reload;
* (v0.1) unregistering or disabling an Agent waits until all its in-flight Handlers have exited and completed their dedup write and Ack before releasing the Consumer binding; no new pulls happen meanwhile.

---

# 8. Dispatch contract

## 8.1 Basic procedure

For one JetStream message, `agentd` performs:

```text
1. Parse the Event Envelope
2. Validate version, event_id, agent_id
3. Verify the target matches the current Agent registration
4. Wait for an available concurrency slot
   (pulls are driven by free slots; agentd never holds
   more messages than it can currently dispatch)
5. Check dedup: the completed store AND the in-flight set
   (v0.1: the dedup decision happens after acquiring
   the slot and before spawning)
6. If the event_id is already in-flight: drop the local
   copy without acking; let JetStream redeliver, then
   dedup will hit via the completed store
7. Spawn the Handler; add the event_id to the in-flight set
8. Concurrently write the original Event JSON to the
   Handler's stdin
   (v0.1: events can be up to 256 KiB, exceeding the
   pipe buffer; the write must run in parallel with
   waiting for exit)
9. Close stdin (EPIPE from an early-exiting Handler is
   normal, not an error)
10. Wait for the Handler process to exit
11. Record the event_id as completed and remove it
    from the in-flight set
12. Ack the JetStream message (double ack)
13. Release the concurrency slot
```

---

## 8.2 Handler input

The Handler receives the complete UTF-8 JSON on stdin:

```bash
/path/to/on-event
```

stdin:

```json
{
  "version": 1,
  "event_id": "...",
  "agent_id": "coding.main",
  "type": "im.message",
  "created_at": "...",
  "payload": {},
  "metadata": {}
}
```

`agentd` does not modify the payload. The original bytes are delivered as received.

---

## 8.3 Handler environment variables

`agentd` MAY provide:

```text
AGENTD_AGENT_ID
AGENTD_EVENT_ID
AGENTD_EVENT_TYPE
AGENTD_STREAM_SEQUENCE
AGENTD_CONSUMER_SEQUENCE
AGENTD_DELIVERY_COUNT
```

These are conveniences only.

stdin remains authoritative for the full Event.

`agentd` must never expose NATS credential contents to Handlers via environment variables.

---

## 8.4 Handler output

v0 defines no stdout protocol for Handlers.

Handlers may log freely.

stdout/stderr may be inherited by `agentd` or forwarded into its own logging system.

---

## 8.5 Handler exit codes

This is one of the most important v0 semantics:

> `agentd` never retries based on Handler exit codes.

Whether the Handler:

```text
exits 0
exits 1
exits 127
is killed by a signal
```

the dispatch is considered finished.

`agentd` should:

1. record the exit status;
2. write the `event_id` into the completed dedup store;
3. ack the JetStream message;
4. not invoke the Handler again.

A Handler that needs retries must implement them itself.

For example:

```python
while True:
    try:
        deliver_to_agent(event)
        break
    except TemporaryError:
        time.sleep(1)
```

Or the Handler can write the event into an Agent-managed queue and exit immediately.

`agentd` does not understand these policies.

---

## 8.6 Handler spawn failure

If the Handler:

* does not exist;
* lacks the executable bit;
* cannot be spawned as a process;
* has a nonexistent working directory;

`agentd` should:

1. log a clear error;
2. mark this dispatch as terminal;
3. ack or term the JetStream message;
4. not retry automatically.

Such errors are local registration or Handler implementation problems, not `agentd` retry problems.

---

## 8.7 Handlers are handoff programs, not whole tasks

The recommended Handler behavior is:

```text
receive Event
→ decide how to find the Agent
→ start the Agent if necessary
→ hand the Event to the Agent
→ return
```

Work that truly spans hours belongs to the Agent Runtime, background jobs, or other services.

A Handler may retry briefly to complete the handoff, but must not host an entire long-running Agent task.

This is not a hard limit. `agentd` v0 sets no Handler timeout.

If an Agent chooses to run its Handler long-term, it accepts:

* occupying one concurrency slot;
* blocking subsequent Events in serial mode;
* owning that lifecycle itself.

---

# 9. Concurrency and ordering

## 9.1 Serial by default

Each Agent defaults to:

```toml
max_concurrency = 1
```

For that Agent, `agentd`:

* pulls one message at a time;
* waits for the current Handler to exit and its Ack to complete before fetching the next;
* therefore preserves Consumer delivery order.

No additional file locking is needed.

---

## 9.2 Concurrent dispatch

An Agent may register:

```toml
max_concurrency = 4
```

Then:

* at most four Handlers run simultaneously;
* `agentd` may hold at most four unfinished messages;
* completion order is not guaranteed;
* finer-grained concurrency control is the Handler's job.

For example, to achieve:

* serialization per repository;
* parallelism across repositories;
* serialization per conversation;

a Handler may use:

* `flock`;
* SQLite;
* Python `filelock`;
* its own dispatcher;
* its own local queue.

None of this enters `agentd`.

---

## 9.3 Stream sequence

JetStream maintains increasing sequence numbers for both Stream and Consumer. Consumer metadata additionally provides delivery counts.

`agentd` may expose these values as environment variables to Handlers, but must not promote them into global ordering semantics of the Agent World.

Under concurrent dispatch, Handlers must not assume completion order matches Stream sequence.

---

# 10. Effectively-once delivery semantics

## 10.1 Goal

During normal operation, one `event_id` should invoke the Handler at most once.

Network reconnects, lost acks, or JetStream redeliveries must not, in the usual case, cause duplicate Handler executions.

---

## 10.2 Minimal dedup state

`agentd` needs no Local Inbox.

It maintains only a small persistent dispatch history, for example SQLite:

```sql
CREATE TABLE completed_events (
    event_id TEXT PRIMARY KEY,
    completed_at INTEGER NOT NULL
);
```

On receiving an Event:

```text
event_id already present
→ do not invoke the Handler
→ ack the message directly
```

After the Handler exits:

```text
INSERT INTO completed_events
→ final Ack
```

Records may be cleaned up by TTL.

Recommended dedup retention exceeds the Stream `MaxAge`, or at least covers common ack-loss and reconnect windows.

## 10.3 In-flight redelivery (v0.1)

The dedup store only records completed `event_id`s. There is a second duplication path that requires no crash: while a Handler runs, the machine suspends or the network partitions for longer than `AckWait`; `in-progress` acks cannot reach the server, and on recovery JetStream redelivers the still-in-flight message — while the original Handler may not have exited yet.

Handling (ADR-0001): `agentd` keeps an in-memory set of in-flight `event_id`s; when a copy of an event that is still in-flight arrives, the local copy is dropped without acking. The server redelivers after `AckWait`; by then the first dispatch has completed, the completed-store dedup hits, and `agentd` acks. The cost is one extra `AckWait`-scale delay on that path; on machine crash the in-flight set is lost and behavior degrades to the known duplicate window of 10.4.

## 10.4 Acceptable duplicate window

The following can still produce duplicates:

```text
Handler has already produced side effects
→ the Handler has not exited yet
→ agentd or the machine suddenly crashes
→ completed_events is not yet written
→ JetStream redelivers
```

v0 accepts this behavior.

The documentation must state clearly:

> `agentd` provides best-effort effectively-once dispatch, not strict exactly-once.

Handlers should implement idempotency via `event_id` where the stakes are high.

For example:

* use the external service's idempotency key when sending email;
* use `event_id` as a unique key when mutating databases;
* deduplicate when injecting messages into an Agent Runtime;
* query target state before deploying.

For ordinary IM messages, one rare duplicate is usually acceptable.

## 10.5 Ack confirmation

The Rust `async-nats` client supports plain acks and double acks that wait for server confirmation; double acking reduces redeliveries caused by "locally considered acked, but the server never saw it".

v0 SHOULD use a double ack after writing `completed_events`.

If the double ack fails:

* do not delete the local `completed_events` record;
* on redelivery, the dedup store skips the Handler;
* simply ack again.

---

# 11. The Handler's full responsibilities

The Handler is the Agent's own userspace Policy.

Everything below belongs to the Handler, not to `agentd`.

## 11.1 Business identity and authorization

A Handler may:

* trust the sender claim of an in-Domain IM Adapter;
* verify end-to-end signatures;
* check allowlists;
* check capability tokens;
* run spam filtering on public messages;
* reject unauthorized requests.

`agentd` checks none of this.

---

## 11.2 Agent health checks

A Handler may:

```text
check a Unix socket
check a PID
call a local health API
check a systemd service
check a container
```

`agentd` has no notion of "Agent online".

---

## 11.3 Waking the Agent

A Handler may:

```text
systemctl --user start pi-agent.service
start a local Python Agent
connect to a resident RPC
start a container
ssh into another machine
ask the cloud to create a new Runtime
```

`agentd` only starts the Handler.

---

## 11.4 Queue / steer / interrupt

Based on the Agent Runtime's capabilities, a Handler may decide to:

* enqueue the Event in the Agent's own queue;
* steer at the next tool boundary;
* create a new Session;
* start a new Worker;
* ignore;
* notify the Human;
* interrupt current work.

`agentd` does not understand these semantics.

---

## 11.5 Retry

If a Handler's handoff may hit transient failures, the Handler retries itself.

`agentd` will never re-invoke a Handler because of:

* a nonzero exit code;
* a failed Runtime startup;
* a Handler exception.

If a Handler prefers to outsource complex retries, it may:

* write to a local queue;
* start a background service;
* submit a Temporal workflow;
* publish a new Event;
* persist failure state and notify the Human.

---

## 11.6 Self-evolution and migration

An old Agent may:

1. deploy the new Runtime;
2. write the new Handler;
3. test the new Handler;
4. call `agentdctl update` to change the registration;
5. future Events start arriving at the new Handler;
6. the old Agent exits.

`agentd` never needs to know a generation handoff happened.

It only ever sees:

```text
same agent_id
→ handler path changed
```

This is the key to `agentd`'s compatibility with Agent self-evolution.

---

# 12. Local trust and the security boundary

## 12.1 v0 threat model

v0 targets:

* single-user Personal Domains;
* multiple Agents under one Unix user;
* trusted local environments;
* self-hosted NATS.

It does not defend against:

* local Agents attacking each other;
* malicious local users;
* a Handler reading other Agents' files;
* a Handler exhausting CPU;
* a Handler modifying `agentd` configuration;
* local multi-tenant isolation problems.

These belong to future versions.

---

## 12.2 Baseline security that must be preserved

Even under default local trust, `agentd` must always:

* receive messages only over the authenticated NATS connection;
* never splice Event fields into a shell;
* execute only absolute Handler paths from the local registry;
* enforce an Event size limit;
* enforce reasonable limits on JSON depth and parsing resources;
* keep the NATS credential file at mode 0600;
* never log credentials;
* never pass NATS credential contents to a Handler;
* (v0.1) recognize the NATS Account as the event-injection boundary — any holder of Domain credentials can publish to any Agent subject; filtering is the Handler's responsibility;
* (v0.1) require Agents that publish to NATS (A2A, replies) to use their own credentials, never `agentd`'s.

---

## 12.3 Runtime privileges

v0 recommends running `agentd` as a user service:

```text
systemd --user
```

Handlers run as the same Unix user as `agentd`.

Running as root is not recommended.

If the future requires:

* switching Unix users;
* starting privileged containers;
* manipulating system-level resources;

add a separate, narrow-interface privileged helper rather than widening the `agentd` core's privileges.

---
# 13. Agent configuration updates and self-management

Agents shall be able to update the local `agentd` configuration.

This is a design goal, not a security hole, because v0 trusts all local Agents.

Typical flow:

```text
Agent modifies its own Runtime
→ writes the new Handler
→ registers the new Handler
→ verifies
→ updates the agentd binding
→ the old Runtime exits
```

An Agent may also:

* register new logical Agents;
* temporarily disable itself;
* change its concurrency;
* point its Handler at a forwarder to another machine;
* delete Agents it no longer uses.

`agentd` does not need to approve any of this.

The Human can always inspect and override via:

```bash
agentdctl list
agentdctl unregister
agentdctl update
```

---

# 14. Runtime and shutdown

## 14.1 Long-term running

`agentd` is expected to stay running most of the time:

* maintaining the NATS connection;
* managing multiple Durable Consumers;
* waiting for Events;
* starting short-lived Handlers.

It must remain lightweight enough to live permanently on:

* desktops;
* laptops;
* small VPSes;
* dev servers.

---

## 14.2 Restarting itself

`agentd` may restart.

Reliability comes from:

* JetStream retaining unacked Events;
* Durable Consumers retaining consumption positions;
* the local completed-event store suppressing common duplicates;
* systemd restarting the daemon automatically.

`agentd` does not need to achieve reliability by never dying.

---

## 14.3 Graceful shutdown

On SIGTERM:

1. stop pulling new messages;
2. wait for running Handlers to exit;
3. finish dedup writes and acks for exited Handlers;
4. close NATS;
5. exit.

v0 may allow systemd to force-kill after the shutdown timeout.

If `agentd` is force-killed while a Handler runs, the message may be redelivered later and the Handler may run twice. This is known, accepted semantics.

---

# 15. Error handling

## 15.1 NATS disconnection

`agentd` must keep trying to reconnect.

While disconnected:

* no new pulls;
* JetStream retains Events;
* locally running Handlers are unaffected;
* consumption resumes after reconnect.

The official Rust `async-nats` client provides NATS, JetStream, TLS, authentication, and reconnection — suitable as the v0 implementation.

---

## 15.2 Invalid Events

The following are Terminal Events:

* unparseable JSON;
* unsupported `version`;
* missing `event_id`;
* missing `agent_id`;
* `agent_id` not matching the Consumer;
* Event exceeding the size limit.

`agentd` should:

1. log the error;
2. ack or term;
3. never retry.

Otherwise one poison Event would block the Consumer forever.

---

## 15.3 Handler failure

A Handler that:

* exits nonzero;
* is killed by a signal;
* throws internally;
* fails to start the Agent;

never triggers an `agentd` retry.

`agentd` only records:

```text
agent_id
event_id
pid
exit_status
duration
```

then completes the dedup write and ack.

---

## 15.4 Stuck Handlers

v0 sets no default Handler timeout.

If a Handler never exits:

* its concurrency slot stays occupied;
* with `max_concurrency = 1`, subsequent messages simply remain in JetStream;
* `agentd` keeps sending in-progress acks;
* a Human or Agent may kill the Handler manually.

A configurable timeout may be added later if a real need appears; it is not a v0 requirement.

(v0.1) No timeout, but `agentd` should log a warning when a Handler exceeds a configurable threshold (default 1h), so Humans notice a long-stuck slot early.

---

# 16. Logging and observability

v0 uses structured logging.

Each log record should include:

```text
timestamp
level
agent_id
event_id
consumer
stream_sequence
handler_path
handler_pid
duration_ms
exit_status
```

Must be logged:

* NATS connect / disconnect;
* Agent register / update / unregister;
* Consumer create / bind;
* Event received;
* Dedup hit;
* Handler spawn;
* Handler exit;
* Ack success / failure;
* Invalid event;
* Spawn failure;
* (v0.1) in-flight duplicate dropped.

Recommended:

```bash
agentdctl status
```

outputting:

```text
NATS connection status
registered agents
consumer lag
active handler count
last event
last error
```

v0 does not need:

* a web UI;
* a Prometheus server;
* distributed tracing;
* agent cognition traces.

---

# 17. Recommended implementation technology

## 17.1 Language

Rust, stable channel.

Reasons:

* a single binary;
* good resource control for a long-lived daemon;
* mature process, signal, and Unix socket support;
* `async-nats` is the official NATS Rust client;
* a good base for future Relay adapters.

This is not an architectural hard requirement, but the implementing Agent must prefer Rust.

## 17.2 Recommended dependency categories

Versions are not frozen; suggested:

```text
tokio
async-nats
serde / serde_json
toml
clap
tracing
rusqlite or equivalent small embedded store
nix or tokio::process
```

Do not introduce:

* a web framework;
* an LLM SDK;
* a workflow engine;
* a plugin framework;
* embedded Python;
* a container runtime SDK.

## 17.3 Code modules

Recommended layout:

```text
src/
├── main.rs
├── config.rs
├── registry.rs
├── control.rs
├── relay/
│   ├── mod.rs
│   └── nats.rs
├── event.rs
├── consumer.rs
├── dispatcher.rs
├── dedup.rs
├── process.rs
├── logging.rs
└── error.rs
```

### `relay/nats.rs`

Owns:

* NATS credentials;
* the JetStream context;
* Streams / Consumers;
* pulling;
* acks;
* in-progress acks;
* reconnection.

### `registry.rs`

Owns:

* AgentConfig;
* the registry;
* reload;
* configuration persistence.

### `dispatcher.rs`

Owns:

* concurrency slots;
* spawning Handlers;
* stdin;
* waiting for exit;
* dedup;
* acks.

### `dedup.rs`

Owns only recent completed `event_id`s.

It is not an Inbox.

---

# 18. Local control protocol

The control socket speaks one JSON request/response per line.

Example:

```json
{
  "op": "register",
  "agent": {
    "agent_id": "coding.main",
    "handler": "/home/clouder/agents/coding-main/on-event",
    "max_concurrency": 1,
    "working_directory": "/home/clouder/projects/main",
    "enabled": true
  }
}
```

Response:

```json
{
  "ok": true
}
```

Other requests:

```json
{"op": "unregister", "agent_id": "coding.main"}
```

```json
{"op": "list"}
```

```json
{"op": "reload"}
```

The protocol needs no version negotiation or other complex machinery.

The socket accepts local connections only.

---

# 19. Example Handler

Below is a conceptual Python Handler.

It is not part of `agentd`.

```python
#!/usr/bin/env python3

import json
import subprocess
import sys
import time


def runtime_is_ready() -> bool:
    result = subprocess.run(
        [
            "systemctl",
            "--user",
            "is-active",
            "--quiet",
            "pi-agent@main.service",
        ],
        check=False,
    )
    return result.returncode == 0


def ensure_runtime() -> None:
    if runtime_is_ready():
        return

    subprocess.run(
        [
            "systemctl",
            "--user",
            "start",
            "pi-agent@main.service",
        ],
        check=True,
    )

    while not runtime_is_ready():
        time.sleep(0.5)


def deliver(event: dict) -> None:
    subprocess.run(
        [
            "/home/clouder/bin/pi-deliver",
            "--event-id",
            event["event_id"],
        ],
        input=json.dumps(event).encode("utf-8"),
        check=True,
    )


def main() -> None:
    event = json.load(sys.stdin)

    # Sender auth, policy, dedup, retry and runtime integration
    # are all local Agent policy.
    ensure_runtime()
    deliver(event)


if __name__ == "__main__":
    main()
```

If `ensure_runtime()` or `deliver()` may fail transiently and the Agent wants retries, implement them inside this script.

`agentd` never re-emits an Event based on a Python exit code.

---

# 20. End-to-end example

## 20.1 Register an Agent

```bash
agentdctl register \
  --id coding.main \
  --handler /home/clouder/agents/coding-main/on-event \
  --max-concurrency 1 \
  --cwd /home/clouder/projects/main
```

`agentd` then:

1. persists the configuration;
2. creates or binds the Durable Consumer;
3. starts pulling `agent.events.coding.main` (id `coding.main`).

---

## 20.2 Publishing a message

An IM Adapter publishes to:

```text
agent.events.coding.main
```

```json
{
  "version": 1,
  "event_id": "01J6ZP8R5EF4Y42KABCD123456",
  "agent_id": "coding.main",
  "type": "im.message",
  "created_at": "2026-08-19T12:00:00Z",
  "payload": {
    "text": "Are the test results out yet?"
  },
  "metadata": {
    "source": "matrix",
    "sender": "@alice:example.com",
    "room_id": "!abc:example.com"
  }
}
```

---

## 20.3 Dispatch

`agentd`:

1. pulls the message;
2. parses the Envelope;
3. dedups;
4. spawns the Handler;
5. writes the JSON to stdin;
6. waits for the Handler to exit;
7. writes `completed_events`;
8. double acks;
9. pulls the next message.

---

## 20.4 The Handler

The Handler:

1. checks the sender;
2. checks the Pi Runtime;
3. starts Pi if necessary;
4. hands the Event to Pi;
5. handles its own transient failures internally;
6. exits.

How Pi replies to the Human afterwards is irrelevant to `agentd`.

Replies may go through:

* Pi calling the IM API itself;
* another outbound worker;
* publishing back to NATS;
* whatever the Agent chooses.

v0 does not unify the Agent output path.

---

# 21. Testing requirements

## 21.1 Unit tests

Must cover:

* Agent ID parsing and subject encoding;
* Event Envelope parsing;
* unknown-field compatibility;
* configuration validation;
* registry updates;
* the dedup store;
* the concurrency gate;
* Handler path validation.

---

## 21.2 NATS integration tests

Must run against a real `nats-server` with JetStream:

### Offline delivery

1. stop `agentd`;
2. publish an Event;
3. start `agentd`;
4. the Handler is invoked exactly once.

### Reconnect

1. `agentd` online;
2. restart NATS;
3. `agentd` reconnects automatically;
4. subsequent Events deliver normally.

### Multiple Agents

1. register Agents A and B;
2. publish Events to each;
3. each Handler is invoked correctly.

### Serial

1. `max_concurrency = 1`;
2. Handler A sleeps;
3. publish several Events;
4. Handlers never overlap.

### Concurrent

1. `max_concurrency = 4`;
2. publish several Events;
3. at most four Handlers run simultaneously.

### Nonzero exit, no retry

1. Handler exits 1;
2. `agentd` records the failure;
3. the message is acked;
4. the Handler is not invoked again.

### Spawn failure, no retry

1. the Handler path is deleted;
2. an Event is published;
3. `agentd` logs a spawn error;
4. the message is terminated;
5. no repeated delivery.

### Ack loss, no duplicate

1. the Handler completes;
2. the dedup record is written;
3. the final Ack is simulated as lost;
4. JetStream redelivers;
5. the Handler does not run again;
6. `agentd` acks again.

### In-flight redelivery (v0.1)

1. a Handler is running;
2. `agentd` is frozen or `in-progress` acks are dropped for longer than `AckWait`;
3. JetStream redelivers;
4. `agentd` does not start a second concurrent Handler;
5. once the original Handler finishes, the redelivered copy hits the completed dedup and is acked.

### Crash window

1. a Handler has started;
2. `agentd` is force-killed;
3. restarted;
4. the Event may be duplicated;
5. the test confirms this matches the documentation.

---

## 21.3 Dynamic configuration tests

Must cover:

* runtime register;
* update Handler;
* unregister;
* disable / enable;
* reload;
* concurrency changes;
* running Handlers survive a reload;
* new Events use the new Handler.

---

# 22. Acceptance criteria

v0 is complete only when:

1. one long-running `agentd` per machine;
2. NATS JetStream connection using credentials;
3. multiple Agents registrable dynamically;
4. one independent Durable Pull Consumer per Agent;
5. Events invoke the correct Handler by `agent_id`;
6. Handlers receive the original JSON on stdin;
7. serial per Agent by default, configurable concurrency;
8. nonzero Handler exit never triggers a retry;
9. spawn failure never triggers an automatic retry;
10. agentd contains no runtime health / wake / queue / steer logic;
11. agentd performs no business sender verification;
12. agentd maintains no local Inbox;
13. agentd maintains the minimal completed-event dedup;
14. consumption resumes after NATS reconnects;
15. an Agent can update its own Handler dynamically;
16. one Agent can switch to a new Handler without restarting the Domain;
17. all behavior is covered by clear structured logs.

---

# 23. Explicit non-goals

v0 does not implement:

* an Agent Harness;
* LLM calls;
* Context management;
* Agent Memory;
* a Runtime adapter framework;
* built-in Pi/Codex/DSH support;
* sender authentication;
* Agent-level IAM;
* capability tokens;
* a local Inbox;
* a local Outbox;
* Handler retries;
* Handler timeouts;
* strict exactly-once;
* a dead letter queue;
* multi-machine leader election;
* multiple active `agentd` instances per Agent;
* automatic Agent migration;
* cross-Domain federation;
* an Agent directory;
* A2A;
* MCP;
* an IM adapter;
* a web UI;
* multi-user sandboxing;
* a Domain-level policy engine.

If implementation reveals a need for any of these, do not add them to the core. Implement them first in a Handler, an external Adapter, or a separate service; discuss abstraction only after a real recurring pattern emerges.

---

# 24. Possible future extensions

The following directions preserve design space but must not block v0:

* additional Relay adapters;
* MQTT / Kafka / HTTP relays;
* multi-machine Agent binding;
* leases and migration;
* Domain-level, multi-user `agentd`;
* Handler sandboxes;
* WASM Handlers;
* Agent presence;
* an outbound Event channel;
* runtime status projection;
* an Agent directory;
* cross-domain contact surfaces;
* stronger local isolation;
* an optional Handler timeout;
* an optional retry policy.

The `agentd` core should remain:

```text
event
→ target lookup
→ executable invocation
```

---

# 25. Final boundary

The entire design compresses into four sentences:

> **JetStream makes events persist while Agents and machines are offline.**

> **`agentd` turns an event into one local executable invocation.**

> **The Handler owns all Agent-specific authentication, wake-up, handoff, retry, and concurrency policy.**

> **The Agent Runtime understands the event and autonomously decides what to do next.**

The value of `agentd` comes not from knowing many Agent concepts, but precisely from knowing almost none.

It does not try to be the Agent's brain, operating system, or control center. It is only the stable, lightweight last stretch of wiring between one machine and the Agent Native Domain:

```text
maybe multiple agents
        ↑
      agentd
        ↕
NATS JetStream
```

This boundary is narrow enough that it never constrains an Agent from later modifying its own Loop, replacing its Harness, migrating to a new machine, or spawning a successor.

And it is practical enough that events from Humans, IM, CI, and other Infrastructure keep waiting while the Agent is not running — and are handed to the local program the Agent left behind, the moment the machine reconnects.

That is everything `agentd` v0 is meant to be.
