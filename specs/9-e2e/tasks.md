# Tasks 009 — E2E suite

- [x] Harness: per-test server/daemon/ports, readiness waits, stream init
- [x] 16 cases per approved matrix
- [x] CI e2e job (nats-server install, AGENTD_E2E=1)
- [x] 16/16 × 3 consecutive local runs
- [ ] PR `Closes #9` merged; CI green on the PR

## Gap-fill round (2026-08-20, user-approved "多多益善")

- [x] §5.4 keepalive: long handler + AckWait < runtime + free slot → no redelivery, no in-flight drop
- [x] §21.3 reload does not interrupt in-flight (SIGHUP mid-handler with a real change)
- [x] §21.3 update → future events use the new handler (end-to-end)
- [x] agentdctl init idempotent against the live server; stream exists after
- [x] §9.1 order preserved at 20 events; and across reload + live register mid-stream (mixed)
- [x] stale socket file replaced at bind (control test)
