# T-086 — Client knows its own connection; track receivers survive id reuse

Status: in-progress
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

### Findings, checked against failing tests
- **Two devices in one channel: wrong premise.** The server keeps one voice
  seat per user. `Rooms::join` removes the user's seat on any other
  connection first (`voice/rooms.rs`, "A second connection for the same user
  is removed from voice first"). Two devices can never both sit in one
  channel. Skipping `JoinVoice` because another device holds the seat in the
  same channel was right by accident. The user-only match was wrong the
  other way:
  - A device that lost the seat (to the other device, while away or while
    connected) rejoined its old channel on its next reconnect, if that was
    another channel than the one the other device used. That took the seat
    back. Reproduced:
    `one_accounts_devices_share_its_voice_seat_across_reconnects` failed
    with "the phone took the seat back".
  - After a moderator `MoveUser`, `desired_voice` kept the old channel. A
    reconnect then moved the member back. Reproduced:
    `a_moved_member_rejoins_the_channel_it_was_moved_to_after_a_restart`
    rejoined `General`, not `Other`.
- **Own track as the user's highest track id.** The rule is wrong, but with
  one seat per user it had no audible effect: a user has at most one audio
  track at a time. A device that lost its seat took the other device's
  track as its own and sent on it, and the server dropped those packets. A
  unit test with two tracks for the account
  (`voice::tests::own_track_is_this_connections_not_the_accounts_newest`)
  picks 7 under the old rule and 5 under the new one.
- **Track id reuse after a restart: confirmed.**
  `a_reused_track_id_gets_a_fresh_receiver_for_its_new_sender` (graph)
  credited the old user. All 15 of the new sender's packets in 300 ms were
  late. The integration test
  `after_a_restart_a_reused_track_id_plays_as_its_new_speaker` showed track
  1 attributed to the departed member and not speaking.
- **Profile switch during a call: confirmed.** A receiver built for one
  frame size rejects every packet of the other with `jitter::Error::Duration`
  (`receivers_follow_the_senders_frame_size_after_a_profile_switch`:
  silent). The server never refreshes seats on a profile change. Old joiners
  keep encoding with the old profile, and new joiners use the new one.

### What changed
- Protocol: `Welcome.conn_id` (bytes, tag 5, `auth.proto`) carries the
  connection id. The server fills it in from the id `attach_voice` returns,
  so a resumed lease keeps the old id. `02-protocol.md` documents the field,
  the one-seat rule and the rejoin rule. No Ack or envelope variants were
  added.
- `State::own_conn` is set on every Welcome. It is `None` when the server
  sends no id.
- `Runner::restore_voice` (`connection.rs`) reads the new snapshot:
  - If this connection still holds a seat, it skips the join and adopts that
    seat's channel and flags (a move during the lease).
  - If another connection of the user holds a seat, it clears
    `desired_voice` and emits `Notice("Voice continued on another device")`.
  - Otherwise it joins.
  - Without `own_conn` it keeps the old user-and-channel match.
- `Runner::follow_own_voice`: a `VoiceStateUpdated` for this connection sets
  `desired_voice` to the new channel, or clears it on a removal. Removals are
  a takeover by the other device, a kick or lost access. `event`, `draining`
  and `execute` now take `&mut self`.
- `load_tracks` (`voice/mod.rs`) takes the track whose `conn_id` is ours.
  Without `own_conn` it falls back to the old rule. A device without a seat
  now gets track 0 and sends nothing.
- `VoiceGraph` (`graph.rs`): `TrackSpec` and `Track` carry the sender's
  `conn`. `sync_tracks` replaces a receiver whose track id now has another
  connection or user. A packet with the wrong frame size makes the graph
  rebuild that receiver for the sender's profile (from the Opus TOC), so old
  and new joiners hear each other after a profile switch.
- CLI load session: `Session::conn_id`, and `Session::own_track` matches the
  connection, or the user on older servers.

### Deviations
- "Rebuild all receivers on `Resynced`" is done by keying receivers on
  `(track id, conn_id)` instead. A restart gives every connection a new id,
  so every receiver is rebuilt. A resume on the same server keeps the
  senders and their sequence numbers, so their receivers are kept and no
  jitter state is thrown away.
- Profile switch: neither a forced rejoin nor a live encoder switch. Each
  receiver follows its sender's frame size. A member keeps encoding with the
  profile it joined with until it rejoins. Switching the encoder live means
  rebuilding the capture path for mono and stereo in `worker.rs`, which
  T-083 owns.
- The first acceptance test is about one account's two devices sharing the
  one seat, not about two seats in one channel. The test covers both
  displacement paths (lost while away, lost while connected) and the seat
  holder coming back after losing the seat with no lease. The test's
  `drop_without_lease` closes a connection as a slow consumer. The server
  then drops the seat at once, which is the state after the 60 s lease, and
  the test does not have to wait a minute.

### Files outside Paths
- `apps/desktop/src-tauri/src/voice.rs` (8 lines): `own_connection` uses
  `own_conn`, and the voice watcher refreshes `ActiveVoice::connection` from
  `own_conn` each tick. Before this, a reconnect that changed the connection
  id (a lease that expired, a server restart) made the watcher think the call
  had ended. It dropped the `VoiceSession` while the core rejoined the seat.
  Screen share's `can_publish` reads the same field. No automated desktop
  test covers it.
- `crates/ss-client-core/src/state/mod.rs`: the `own_conn` field (10 lines).
- `crates/ss-client-core/src/voice/worker.rs` (T-083's file): one line,
  `conn` in a test's `TrackSpec` literal.
- `crates/ss-cli/src/load/bot.rs`: one line, the call to `own_track`.
- `crates/ss-server/src/lib.rs` (the voice spike's Welcome),
  `crates/ss-proto/tests/frame_round_trip.rs` and
  `crates/ss-client-core/src/connection/pump_tests.rs`: the new field in
  struct literals.
- `crates/ss-server/tests/client_core.rs`: the three integration tests.

### Tests
- New: the three integration tests in `ss-server/tests/client_core.rs`
  named above, plus three unit tests (two in `graph.rs`, one in
  `voice/mod.rs`). Each failed before the fix for the reason given.
- `just check` passes.

### Follow-ups
- `crates/ss-cli/src/call.rs` (`note_track`) still finds its own track by
  user id.
- The server's `Rooms::join` removes the other seat and the current one
  before it checks the user limit. A `JoinVoice` into a full channel then
  fails with those leave events unpublished. Not checked further.
- A member who joined before a profile switch keeps the old encoder (and the
  old seat bitrate cap) until they rejoin. The desktop could rejoin on a
  `ChannelUpdated` for the joined channel.

### For hands-on testing
1. Two devices on one account, A in channel X. Join Y on B: A leaves voice,
   and its UI shows it. Turn A's network off for over a minute and back on:
   A stays out of voice, and B keeps the seat and its audio.
2. One device in a call: turn the network off for over a minute, then back
   on. It rejoins, and others hear it.
3. Restart the server during a call with three or more people. Each must
   rejoin in a different order than they first joined. Speaking indicators
   and per-user volume must follow the right person, and everyone is heard
   as soon as they show as connected.
4. Desktop: restart the server during a call. The call stays up in the UI
   and audio comes back.
5. Switch a channel between `voice` and `low_latency` during a call, then
   have someone join. Old and new members hear each other.

