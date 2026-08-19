# Tasks 004 — Dedup store

- [x] ADR-0005 + whitepaper §10.2/§10.3 amendment (EN + zh)
- [x] `rusqlite` (bundled) dep; `resolved_dedup_path()` + test; `AgentdError::DedupStore`
- [x] `src/dedup.rs`: open/is_completed/mark_completed/purge_expired; WAL + FULL + busy_timeout
- [x] Tests: hit/idempotent/TTL/composite-key isolation/concurrency/file-backed open
- [x] `just lint` + `just test` green (43/43)
- [ ] PR `Closes #4` merged
