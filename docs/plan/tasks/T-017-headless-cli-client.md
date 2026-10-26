# T-017 — `ss-cli`: headless client with latency measurement and impairment modes

Status: todo
Phase: 1
Size: M
Depends on: T-010, T-014, T-015, T-016
Paths: `crates/ss-cli/src/`

## Goal
A terminal client that can actually be used for a voice call between two
machines, measures mouth-to-ear latency, and simulates bad networks. It stays
useful later for load testing and debugging.

## Scope
- `ss-cli connect <host:port> --name X --channel N [--profile voice|music]`:
  pin prompt (TOFU), join, full audio graph (T-010 + T-014 + T-015), PTT via
  spacebar in the terminal or `--vad` (simple energy gate) or `--open-mic`.
- Live stats line: RTT, loss, jitter, buffer delay per speaker, underruns,
  bitrate.
- `--measure-latency`: plays a periodic click into the *send* path and
  cross-correlates it in the *receive* path of a second instance looped back
  through the server, printing mouth-to-ear latency using cpal timestamps.
- `--impair loss=5,jitter=30,reorder=1`: applies loss/jitter to outgoing
  datagrams in-process (so no `tc` needed).
- `--bots N`: spawn N synthetic clients sending pre-recorded Opus (for T-062).
- `--record out.wav` of the mixed output for A/B listening.

## Out of scope
Text chat, GUI.

## Acceptance criteria
- Two people on two machines can talk via a VPS-hosted T-016 server.
- `--measure-latency` reports a stable number (documented) on a LAN and via
  the VPS.
- `--impair loss=5` is audibly acceptable (PLC/FEC working); stats confirm.

## Notes from implementation
