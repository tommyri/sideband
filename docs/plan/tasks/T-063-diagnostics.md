# T-063 — Diagnostics: client panel, server metrics, log bundle

Status: in-progress
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


Implemented (2026-11-03):
- Panel via audio settings and Ctrl+Shift+D. Built-in deafen moved to
  Ctrl/Cmd+Shift+Alt+D to resolve the previous shortcut collision; configured
  global shortcuts are unchanged.
- Shared core on-demand Quinn diagnostics and voice-worker projection: RTT,
  cumulative packets sent/lost, cwnd, path MTU/datagram ceiling; actual CPAL sample
  rates/backend; per-speaker delay/loss/PLC/late; encoder/profile FEC; measured
  software stage processing time, ring counters and input backend/events.
- 60-second/120-sample graphs, gaps for unavailable/delayed samples, no stale
  values across server changes; polling stops on close. Migration count, negotiated
  hardware buffer sizes, Apple system CPU/rates and acoustic latency remain
  explicitly unavailable where the current APIs expose no measurement.
- T-076 recorder/schema/redaction/export reused for preview, clipboard and local
  ZIP bundles. No raw app logs, user IDs, addresses, credentials or recordings.
- Loopback-only `/connections` and `ss-server conn list`; no proxy/redirect use.
  Grafana dashboard JSON and scrape/import instructions in `deploy/grafana/`.
  The optional member-list quality dot was not added.

Validation: bounded history/gaps and redaction tests; UI export/close lifecycle and
shortcut-conflict tests; synthetic core/desktop transport/audio integration; real
server CLI test verifies live counters and absence of credentials/addresses.

Packaged macOS smoke: Ctrl+Shift+D opened the disconnected-state panel without
permission prompts; clipboard copy succeeded; ZIP export passed an independent
CRC/content check with a seeded token absent from every entry. The visual check
also fixed scrolling/preview compression in the diagnostics and guided-test
report dialogs, which use a document layout rather than audio settings' split body.

Remaining acceptance: compare desktop and CLI observations on the same real call
with sampling tolerance, validate hardware routes on Windows/macOS/Linux, and
import the dashboard into a running Grafana/Prometheus deployment. These checks
remain manual; numerical packet counters do not establish audible voice quality.
