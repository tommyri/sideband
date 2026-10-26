# T-016 — Minimal voice forwarding server (spike)

Status: todo
Phase: 1
Size: S
Depends on: T-004
Paths: `crates/ss-server/src/` (spike module, kept and grown in phase 2)

## Goal
A server that accepts QUIC connections, lets a client "join channel N" over
the control stream, and forwards audio datagrams to the other members of that
channel. This is the seed of the real media router.

## Scope
- Binary `ss-server` with `--data-dir`, `--bind`; generates a self-signed
  cert on first run and prints its fingerprint.
- Accept `Hello` with any username (no auth yet), reply `Welcome` with an
  empty snapshot.
- Handle `JoinVoice{channel_id}` / `LeaveVoice`; assign `track_id`s; send
  `TrackAdded/TrackRemoved` and `VoiceStateUpdated` to channel members.
- Datagram path: parse header, validate track ownership, fan out to channel
  members via `send_datagram`; count drops.
- Structured as `media::Router` and `voice::Registry` modules that T-025 will
  keep.

## Out of scope
Auth, persistence, text, permissions.

## Acceptance criteria
- Integration test: 3 in-process clients join the same channel; datagrams
  from A reach B and C and not A; a 4th client in another channel receives
  nothing.
- Forwarding path does no allocation per datagram (reuse `Bytes` slice).
- 20 clients × 50 pps sustained on a laptop with < 5 % of one core
  (rough check, numbers in notes).

## Notes from implementation
