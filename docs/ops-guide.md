# Ops Guide

For humans operating `agentd` and its relay. Agents have their own
[guide](agent-guide.md); the [whitepaper](whitepaper-v0.md) is the
specification of record.

---

## Install

From crates.io (easiest):

```bash
cargo install agent-daemon
```

Prebuilt binaries (`agentd` + `agentdctl`, static musl and gnu, x86_64 and
aarch64) from [GitHub Releases](https://github.com/Clouder0/agent-daemon/releases),
with `SHA256SUMS`. Or from a checkout of the source:

```bash
cargo install --git https://github.com/Clouder0/agent-daemon
```

v0 supports Linux. Handlers run as the same Unix user as the daemon.

## Relay setup (once per domain)

1. Run a NATS server with JetStream (file storage):

   ```bash
   nats-server -js -sd /var/lib/nats/js
   ```

2. Create the stream — operator-time, one-shot (ADR/v0.1):

   ```bash
   agentdctl init                       # uses agentd.toml defaults
   agentdctl init --creds /etc/agentd/operator.creds --url nats://relay:4222
   ```

   The **running daemon never creates the stream**; its credentials can stay
   consumer-only.

### Credentials (§3.4)

- One `.creds` file per machine; treat like a password (contains an NKey
   seed); **mode 0600** — agentd warns at startup if it is looser.
- The NATS account is the domain's trust boundary: anyone with domain
   credentials can publish to any agent subject. Filtering is the
   handler's job (§12.2).
- Agents that publish (A2A, replies) must use **their own** credentials,
   never the daemon's.

## Configuration

`agentd.toml` — default `$XDG_CONFIG_HOME/agentd/agentd.toml`
(`agentd --config PATH` overrides; a missing default file means "use
defaults", noted loudly):

| Key | Default | Meaning |
|---|---|---|
| `nats_url` | `nats://127.0.0.1:4222` | Relay URL. |
| `nats_creds` | — | Credentials file (0600). |
| `stream_name` | `AGENT_EVENTS` | JetStream stream (created by `agentdctl init`). |
| `agents_dir` | `$XDG_CONFIG_HOME/agentd/agents.d` | Per-agent TOMLs (`<agent_id>.toml`). |
| `control_socket` | `$XDG_RUNTIME_DIR/agentd/control.sock` | Control socket (0600; data-dir fallback + warning when no runtime dir). |
| `dedup_path` | `$XDG_DATA_HOME/agentd/dedup.db` | Completed-event dedup store. Corrupt store = startup refusal, never silent reset. |
| `dedup_ttl_days` | 14 | Dedup retention (> stream MaxAge 7d). |
| `max_event_bytes` | 262144 | Envelope size cap (also the stream's max message size; ≤ i32::MAX). |
| `ack_wait_secs` | 300 | Consumer AckWait (ADR-0001). |
| `ack_progress_interval_secs` | 90 | In-progress keepalive cadence while a handler runs (§5.4). |
| `slow_handler_warn_secs` | 3600 | WARNING threshold; **not** a timeout. |

Agent registration files (`agents.d/<agent_id>.toml`) are managed by
`agentdctl`/the socket — hand edits work too; `SIGHUP` (or
`agentdctl reload`) picks them up. Unknown keys are rejected in both
formats (typos fail loudly).

## systemd (recommended)

`~/.config/systemd/user/agentd.service`:

```ini
[Unit]
Description=agentd — Agent Native Domain edge daemon
After=network-online.target

[Service]
ExecStart=%h/.cargo/bin/agentd run
Restart=always
# Graceful shutdown: agentd waits for in-flight handlers (never kills them);
# this is the backstop for the pathological case.
TimeoutStopSec=300

[Install]
WantedBy=default.target
```

## Running & observing

```bash
agentd run                              # foreground; JSON logs on stdout
RUST_LOG=debug agentd run               # overrides the configured level
journalctl --user -u agentd -f          # under systemd
agentdctl status                        # version, nats state, per-agent backlog
agentdctl list
```

Log lines are JSON with the §16 field set; dispatch lines carry the full
`dispatch > handler` span chain (`agent_id`, `event_id`, `consumer`,
`stream_sequence`, `handler_path`, `handler_pid`, `duration_ms`,
`exit_status`).

Shutdown (SIGTERM/Ctrl-C): stop pulling → wait for every in-flight handler
to finish and ack → remove the socket → exit 0.

## Troubleshooting

| Symptom | Meaning / action |
|---|---|
| `nats disconnected` repeating | Relay unreachable; the daemon retries forever and resumes within ~10s of the relay returning (client-capped fetch). |
| `consumer bind failed … stream not found` | Run `agentdctl init`. That agent stays un-consumed until the next reload applies. |
| `Waiting Pulls: 2` on the server | Two daemons share one consumer — the §5.3 misconfiguration. Stop all but one (`pgrep -x agentd`). |
| `handler exceeded the slow-handler warning threshold` | A slot is held for > `slow_handler_warn_secs`. Not an error; investigate the handler. |
| `PENDING` grows in `status` | Serial agent slower than its event rate, or the agent is disabled/daemon not pulling. |
| `dedup hit` storms | Same `event_id` republished (senders reusing ids); harmless but fix the sender. |
| Events lost while the daemon was down for > 7 days | Stream MaxAge expiry — retention is a stream setting; raise it if you need longer offline windows. |

## Scale & behavior notes (deliberate trade-offs)

- One durable fsync per completed event (dedup durability); comfortably
  above personal-domain event rates, by design.
- Up to two payload copies per event (delivery + keepalive handle) — bounded
  by `max_event_bytes`.
- Handlers inherit agentd's full environment (same-user trust, §7.3): do
  not run the daemon with secrets in its environment.
- The control socket trusts same-user connections (no auth beyond 0600,
  §7.3).

## Security model (summary)

Trusted: same Unix user, the domain's NATS account. Unverified: envelope
senders (any credential holder can publish; handlers filter), handler
liveness, business semantics. Enforced: no shell interpolation (execve of
absolute paths only), envelope size + version validation, credential
contents never logged or passed to handlers.
