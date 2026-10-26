# T-025 — Voice membership, media router with permissions

Status: todo
Phase: 2
Size: M
Depends on: T-016, T-022, T-024
Paths: `crates/ss-server/src/voice/`, `crates/ss-server/src/media/`

## Goal
Grow the T-016 spike into the real voice subsystem: permission-checked join,
mute/deafen state, server mute, user limits, track lifecycle, and the
forwarding router.

## Scope
- `JoinVoice`: requires `VIEW_CHANNEL` + `CONNECT`, channel kind voice,
  `user_limit` not exceeded (bypass with `MOVE_MEMBERS`); switching channels
  is atomic (leave + join); a user may be in voice from one connection only
  (joining from a second connection moves them).
- `SetVoiceState` self mute/deaf; `ServerMuteUser` (T-030 wires the handler,
  the state lives here).
- Track allocation: `track_id` per (conn, kind), reused never within a
  session; `TrackAdded` includes codec params derived from the channel's
  `VoiceProfile`; `TrackRemoved` on leave/disconnect.
- Router: forward only if sender has `SPEAK` (cached per membership, refreshed
  on role change), not server-muted; skip self-deafened and the sender; count
  `forwarded/dropped/rejected`. Expose a `RouterStats` snapshot.
- Membership snapshot for `Welcome` and `VoiceStateUpdated` events on every
  change.
- Disconnect cleanup within 1 s of QUIC close.

## Out of scope
Video streams (T-070), moderation commands (T-030).

## Acceptance criteria
- Tests: join without `CONNECT` → `FORBIDDEN`; server-muted user's datagrams
  not forwarded; deafened user receives none; user limit enforced; switching
  channels emits exactly two `VoiceStateUpdated`.
- Router hot path still allocation-free (reuse T-016 check).

## Notes from implementation
