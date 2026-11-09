# T-086 — Client knows its own connection; track receivers survive id reuse

Status: todo
Phase: 4
Size: M
Depends on: T-081, T-082
Paths: `crates/ss-proto/proto/socialspeak/v1/auth.proto`, `crates/ss-server/src/connection.rs`, `crates/ss-client-core/src/connection.rs`, `crates/ss-client-core/src/voice/mod.rs`, `crates/ss-client-core/src/voice/graph.rs`, `crates/ss-cli/src/load/session.rs`, `docs/plan/02-protocol.md`

## Goal
Two devices on one account behave correctly in voice, and a server restart
does not leave clients decoding one user's audio as another's.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
reproduced. Touches files T-081 and T-082 change; run after both.

## Scope
- Voice state is keyed by `conn_id` and the architecture allows several
  connections per user, but `Welcome` carries no `conn_id` and the
  `JoinVoice` Ack is empty. The client matches on user id only. After a
  reconnect past the 60 s lease, `already_joined` is true because of the
  other device's seat, `JoinVoice` is skipped and this device is silently
  out of voice. Add `conn_id` to `Welcome` with a new tag (never reuse a
  tag) and match on it.
- The client picks its own audio track as the highest track id for its own
  user (`voice/mod.rs`, also the CLI load session). Use the connection id.
- Track receivers are keyed by track id only. The server's `next_track`
  restarts from zero after a restart, so ids map to different users;
  `sync_tracks` updates gain and mute but not `user` and keeps the old
  jitter state. Speaking indicators and per-user volume then hit the wrong
  person, and the new sender's packets are late until its sequence passes
  the old committed one. Recreate the receiver when the user changes and
  rebuild all receivers on `Resynced`.
- A channel's voice profile is read once at join. Check what happens when
  an admin switches a channel to or from `low_latency` during a call
  (reported, not confirmed: old and new joiners drop each other's packets on
  duration mismatch) and either force a rejoin or apply the change live.

## Out of scope
Per-device presence in the UI.

## Acceptance criteria
- Integration test: two connections for one user in one channel; one
  reconnects after the lease expires and is back in voice with its own
  track.
- Integration test: server restart with members rejoining in a different
  order; each receiver decodes the right user's audio at once.
- Old clients ignore the new field; `02-protocol.md` documents it.

## Notes from implementation
