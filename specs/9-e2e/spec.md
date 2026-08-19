# Spec 009 — E2E suite (§21.2 matrix)

## Goal

Prove v0 semantics against a real nats-server with JetStream and the real
`agentd` binary — the evidence behind the §22 acceptance walk-through.

## Harness

tests/e2e.rs, gated by `AGENTD_E2E=1` + nats-server on PATH; unique OS port
per Env (bind :0); server readiness wait; stream created per test
(operator-time step, as `agentdctl init`); daemon ready = control socket
answers List. CI installs nats-server v2.11.6 and runs the suite.

## Cases (16)

Delivery: offline delivery; relay-restart recovery (bounded by FETCH_CAP);
multi-agent no-crosstalk; byte-exact stdin.
Ordering/concurrency: serial gate (strict start/stop alternation);
concurrent gate (3 × 0.5s finish < 1.3s).
Terminal: exit-1 single run; spawn failure logged; invalid envelopes (×3);
oversize.
Effectively-once: dedup across restart + republish; in-flight duplicate
dropped (concurrency > 1 so the copy is fetched during flight — ADR-0001);
crash window (SIGKILL mid-handler; 1–3 runs; settles).
Lifecycle: live register via socket dispatches without restart; SIGHUP
disable stops consuming; SIGTERM drains in-flight, exits 0.

## Approved deviations

Exact ack-loss injection (impractical externally) folded into the dedup
cases; the crash window asserts the documented range, not exactly-1.

## Acceptance

- [x] 16/16 green locally × 3 consecutive runs (~15 s each)
- [x] CI e2e job wired (nats-server install + AGENTD_E2E=1)
