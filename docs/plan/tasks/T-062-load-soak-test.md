# T-062 — Load and soak testing

Status: todo
Phase: 4
Size: S
Depends on: T-017, T-025, T-026
Paths: `crates/ss-cli/src/bots.rs`, `docs/plan/notes/load-test.md`

## Goal
Know the server's envelope on the actual VPS and catch leaks before users do.

## Scope
- Extend `ss-cli --bots N`: N synthetic users across M channels, each
  speaking in bursts (pre-encoded Opus) with configurable duty cycle, sending
  text at a rate, joining/leaving periodically.
- Scenarios: 25 users / 3 voice channels for 2 h; 100 users / 10 channels
  for 15 min (headroom); reconnect storm (all disconnect and reconnect).
- Measure on the VPS: CPU, RSS, datagram drops, forwarding latency (server
  timestamp delta), SQLite write latency, open file descriptors; watch for
  monotonic growth (leaks).
- Produce `docs/plan/notes/load-test.md` with graphs (export Prometheus →
  PNG via a script) and tuning outcomes (UDP buffer sizes, tokio worker
  count, SQLite cache).

## Out of scope
Horizontal scaling.

## Acceptance criteria
- 25-user soak: no growth in RSS > 10 % over 2 h, zero forced disconnects,
  < 0.1 % datagram drops.
- Numbers for 100 users recorded with the bottleneck identified.

## Notes from implementation
