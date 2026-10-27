# T-025 — Voice membership, media router with permissions

Status: done
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

- The T-016 spike stays the default listener. Its registry and router moved to
  `voice/spike.rs` and `media/spike.rs` and are still `voice::Registry` and
  `media::Router`. Authenticated membership is `voice::Voice`, sharing one
  `parking_lot` mutex with `media::Forwarder`. `connection::Server::media_stats`
  exposes the four counters.
- `JoinVoice` requires `VIEW_CHANNEL`, then `CONNECT`, on a live voice
  channel. A missing or deleted channel is `NOT_FOUND`. Anything else is
  `INVALID` ("not a voice channel"). `user_limit` is checked inside the lock
  after any leave; absent and `0` mean no cap. `MOVE_MEMBERS` bypasses the
  cap, which is how an administrator still enters a full or connect-denied
  channel. Joining the same channel updates mute and deaf and keeps the
  track. A switch is one leave plus one join under that lock, so observers
  see exactly two `VoiceStateUpdated` events. A second connection for the
  same user is moved out of voice first.
- Members already in the channel are announced only on the joiner's command
  frames. Leave, join, mute, and track events are also published on the bus.
  `SetVoiceState` outside voice is `INVALID`. `LeaveVoice` outside voice is
  an empty ack. `ServerMuteUser` requires `MUTE_MEMBERS` and remembers the
  user until process restart, emitting `VoiceStateUpdated` when they are in
  voice. Audit and hierarchy stay in T-030. `SPEAK` is cached on the seat
  and refreshed after every successful role or channel command.
- Deleting a channel appends `TrackRemoved` and a `VoiceStateUpdated` with
  no channel for each voice member of that channel. `Welcome` copies voice
  states and tracks the viewer can `VIEW_CHANNEL`, and advertises `voice`.
  The session reads datagrams beside the control stream. QUIC close ends the
  session; the voice guard then publishes the leave. That cleanup is the
  session ending, not a one-second timer.
- The forwarder rejects a bad header, an unknown sender, the wrong track, a
  self-mute, a server mute, or a missing `SPEAK`. It skips the sender and a
  self-deafened peer without counting those skips as rejections. Payloads
  stay shared `Bytes`. The T-016 allocation check still covers `Router::forward`.
  Track ids start at 1, are never reused, and stop at `MAX_TRACK_ID`. The
  announced codec is Opus at 48 kHz with the channel count from the channel
  profile.
