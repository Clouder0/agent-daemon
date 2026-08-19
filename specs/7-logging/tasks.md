# Tasks 007 — Structured logging

- [x] `specs/7-logging/` written (this folder)
- [x] Issue #7 re-scoped (status → #6); #6 updated
- [x] `src/logging.rs`: `init`, `build_filter`, `events` helpers, `dispatch_span`
- [x] `main.rs` wires logging into `agentd run` (verified: JSON line on stdout)
- [x] Tests: filter precedence ×4, span/helper field capture (25 total, green)
- [x] `just lint` + `just test` green
- [ ] PR `Closes #7` merged
