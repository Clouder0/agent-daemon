# Tasks 002 — Relay + run assembly

- [x] ADR-0006 + unified-`_` sweep (whitepaper EN/zh, agent_id.rs, all tests/examples, README, AGENTS)
- [x] `src/relay.rs`: connect (creds + retry), ensure_stream, consumer_name/config, NatsAcker (double ack/term), Relay bind/pull/apply_changes/shutdown, per-dispatch keepalive
- [x] `agentd run`: --config, wiring, SIGHUP reload, SIGTERM/Ctrl-C graceful
- [x] Unit tests (naming/config identity); 65/65 green
- [x] Live docker smoke vs nats-server
- [ ] PR `Closes #2` merged
