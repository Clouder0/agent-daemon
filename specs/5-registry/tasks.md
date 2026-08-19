# Tasks 005 — Agent registry

- [x] Dot-separated agent ids: ADR-0004 + sweep (agent_id.rs, whitepaper §2.3/§5.2/§7/§8/§20, examples, README, zh ref)
- [x] `dirs` dep added; `config.agents_dir` (default `$XDG_CONFIG_HOME/agentd/agents.d`)
- [x] `src/registry.rs`: AgentConfig + two-tier validation, load/register/update/unregister/set_enabled, persist-then-move, reload diff, `Change`
- [x] Tests: validation, duplicate, filename-match, reload transitions, roundtrip, update/unregister flow
- [x] `just lint` + `just test` green (34/34)
- [ ] PR `Closes #5` merged; Copilot review triaged
