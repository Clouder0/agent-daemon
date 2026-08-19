# Tasks 006 — Control plane

- [x] src/control.rs: DaemonHandle, RelayBackend seam, Request/Response, bind/serve
- [x] Dispatcher::in_flight; Relay::consumer_backlog + RelayBackend impl; connect() connected-flag
- [x] main.rs rewire: Arc<Relay>, handle, control task, socket cleanup
- [x] agentdctl: init (+creds/url), register/update/unregister/list/reload/status
- [x] tests/control.rs (7); live smoke (9 steps)
- [x] 72/72 green, lint clean
- [ ] PR `Closes #6` merged
