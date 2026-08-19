# Plan 009 — E2E suite

- **Status:** Approved (matrix + deviations ratified in discussion)
- **Issue:** #9 (`Closes #9`)

## Verification

Three consecutive 16/16 local runs against a real nats-server (binary from
the official image); CI runs the same suite.

## Harness lessons

nextest isolates each test in its own process — a static port counter
collides (allocate via bind :0); the harness creates the stream before
daemon start (operator-time step); the server needs a listen-wait after
spawn; a SIGTERM helper must not wait() before signaling.
