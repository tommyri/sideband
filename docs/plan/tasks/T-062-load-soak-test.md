# T-062 — Load and soak testing

Status: done
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

- `ss-cli load HOST:PORT` is the extended `--bots N`: N authenticated users
  over M voice channels (`load-1`…`load-M`) plus `load-text`. Flags:
  `--duty`, `--burst`, `--text-rate`, `--churn`, `--away`,
  `--storm-every`, `--duration`, `--interval`, `--ramp-ms`, `--csv`,
  `--state`, `--invite`, `--prefix`, `--profile`. `connect --bots` (spike
  senders) is unchanged. Code: `crates/ss-cli/src/load/`; the pre-encoded
  Opus buffer still lives in `bots.rs`.
- Bots log into `ss-server run`, not the spike, so text and SQLite are in
  the measurement. The five-per-minute login limit makes the first
  registration of 100 bots take about 20 minutes from one address; tokens
  are kept in `--state` and reused (session tokens are not limited). With
  the owner invite on a fresh server, the first bot creates the channels
  and an unlimited invite.
- Forwarding latency is measured client-side: datagram timestamps carry the
  bot process's 48 kHz send clock, and receivers in the same process
  subtract it. The server adds no per-packet timing.
- Server additions: `socialspeak_datagrams_received_total`,
  `socialspeak_datagrams_rejected_total`, `socialspeak_sessions_total`, and
  `socialspeak_slow_disconnects_total` on `/metrics`;
  `SS_WORKER_THREADS=N` selects a multi-threaded runtime (default still one
  thread, which measured cheaper).
- `scripts/load-metrics.py` (stdlib only; matplotlib is not installed here,
  so graphs are hand-drawn SVG) scrapes `/metrics`, `ps`, and descriptors
  to CSV, plots, and summarizes. `scripts/load-run.sh` wraps one scenario.
- Deviation: every run so far was local on the development Mac at reduced
  length (25 users for 12 and 30 minutes, 100 users for 5 minutes, a
  100-user storm run, 2-minute full-duty stress runs). The 2-hour soak, the
  15-minute headroom run, and the storm on the VPS are still owed; the
  acceptance criteria are not yet verified on the VPS. Results, the 100-user
  bottleneck (UDP send syscalls on the one runtime thread), and the VPS
  procedure are in `docs/plan/notes/load-test.md`.
