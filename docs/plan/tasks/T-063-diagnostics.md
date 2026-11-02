# T-063 — Diagnostics: client panel, server metrics, log bundle

Status: todo
Phase: 4
Size: S
Depends on: T-041, T-044, T-032, T-076
Paths: `apps/desktop/src/features/diagnostics/`, `crates/ss-client-core/src/diagnostics.rs`

## Goal
When someone says "you sound robotic", anyone in the group can find out why
in a minute.

Reuse T-076's local collector, redaction and versioned export format. This
task adds the broader diagnostics panel/graphs and server dashboards; the
testing-app milestone does not wait for these extensions.

## Scope
- Client diagnostics panel (Ctrl+Shift+D): connection (RTT, loss, cwnd,
  path MTU, migrations), audio devices + backend + buffer sizes + measured
  latency, per-speaker jitter buffer delay/loss/PLC/late, own encoder
  bitrate/FEC state, processing stages on/off with CPU, underruns, PTT
  backend and last events. Live graphs for the last 60 s.
- "Copy diagnostics" → redacted text blob; "Save log bundle" → zip of
  recent logs + diagnostics JSON.
- Server: Grafana dashboard JSON in `deploy/grafana/` for the T-032
  metrics; per-connection stats in `ss-server conn list`.
- Optional per-channel "voice quality" dot in the member list derived from
  loss/jitter thresholds.

## Out of scope
Remote telemetry (none, ever, by default).

## Acceptance criteria
- Panel values match `ss-cli` stats within tolerance on the same call.
- Log bundle contains no tokens or passwords (test with a grep in CI over a
  generated bundle).

## Notes from implementation
