# T-103 — Voice membership reconciliation moves to ss-client-core

Status: in-progress
Phase: 3
Size: M
Depends on: T-084, T-089, T-095
Paths: `crates/ss-client-core/src/voice/`, `apps/desktop/src-tauri/src/voice.rs`

## Goal
Following a moderator move, dropping a call whose seat is gone and pausing
PTT while reconnecting behave the same in every host and are tested
without Tauri.

Source: T-095. `voice::observe` in the shell runs every 40 ms and decides
when the seat moved (restart the session muted in the new channel with its
profile), when it is gone (end the call) and when the connection is
retrying (release PTT). It needs `VoiceSession`, which T-084 owns, so T-095
did not move it.

## Scope
- Give `VoiceSession` (or a small owner type next to it) the reconcile
  step: it reads the call's own `StateStore`, tracks the connection id across
  a non-resumed reconnect, and reports `Follow { channel }`, `Ended` or
  `Retrying`, or performs the follow itself and reports it.
- The shell keeps the watch timer, the `ss://voice-session` and
  `ss://speaking` events, and the `operations` serialization.

## Out of scope
Changing the follow-muted rule. Voice command locking (T-085).

## Acceptance criteria
- An `ss-server` integration test moves a member and sees the client
  follow muted into the new channel without the desktop crate.
- `voice::observe` in the shell has no membership branches.

## Notes from implementation

Implemented 2026-11-11. The integration tests were written first and did not
compile (no `reconcile`, `follow`, `connection` or `Membership`). After the
move, removing each rule in turn (the PTT release, the forced mute, the
closed-client check) failed the test that covers it.

**Design: report, then follow in core.** `VoiceSession::reconcile(&self) ->
Membership` is synchronous and cheap. It reads only the call's own
`StateStore`, tracks the connection id, and returns `Seated`, `Retrying`,
`Follow { channel }` or `Ended`. On `Retrying` it releases PTT itself. The
follow is a separate core step: `VoiceSession::follow(self: Arc<Self>,
client, channel, config)`. It forces `start_muted`, starts in the
destination (whose profile and bitrate `start` reads), and keeps the host's
cue switches. So the "restart muted with the new channel's profile" rule
lives in one tested place. The follow is not done inside `reconcile`. The
desktop calls `reconcile` under its voice lock every 40 ms, and the follow
is a network round trip plus a device open. Holding the lock across it would
make PTT and mute wait, which the shell avoids on purpose. The host takes
the call out of its slot, drops the lock and calls `follow`.

**What moved** (`voice/membership.rs`, `voice/mod.rs`):
- The membership decision `membership::membership(state, tracked, channel)`,
  a pure function with unit tests:
  - seated vs. moved;
  - missing seat, or `Disconnected`/`Connecting`/`Failed`, is `Ended`;
  - `Retrying` is reported before the seat is read;
  - a new `own_conn` is tracked;
  - older servers without `own_conn` keep the joined connection.
- Connection tracking: `VoiceSession::connection()`. `start` takes the id
  from the own track that `load_tracks` found (`Tracks::own_conn`). It is
  `own_conn`, or the newest own track's connection on older servers, which
  was the desktop's `own_connection`.
- A stopped client (`commands.is_closed()`, after `Client::abort`) ends the
  call. The desktop used to get this by not finding the client in its map;
  the store of an aborted client that is still shared can say `Connected`.

**Shell** (`voice.rs`): `observe` matches on `reconcile()` and only acts:
- speaking users;
- `active.release_ptt()` so its two PTT inputs reset too;
- drop the call and emit `ss://voice-session`;
- `follow`: the saved audio config, `session.follow`, `ss://voice-error` on
  failure.

It no longer reads `voice_states`, `own_conn` or the connection state.
`ActiveVoice::connection` is now a method over the session's tracked id, and
`own_connection` is gone. The 40 ms timer, both events and the `operations`
try-lock are unchanged.

**Tests** (`crates/ss-server/tests/client_core.rs`, no desktop crate):
- `a_moved_member_follows_muted_into_the_new_channel_and_its_profile`:
  - a moderator moves the guest into a music-profile channel;
  - `reconcile` reports `Follow`;
  - after `follow` the call is muted locally and on the server, sends
    nothing, and encodes at the music default of 128 kb/s.
- `a_call_ends_when_its_seat_or_its_client_is_gone`: another device of the
  account takes the seat while the phone stays connected, and a client is
  aborted.
- `retrying_releases_ptt_and_a_new_connection_id_keeps_the_call`:
  - PTT held, then the seat is dropped without a lease;
  - `Retrying` releases the gate at once;
  - after the reconnect the call is `Seated` on a new connection id that
    `connection()` reports, and the held key stays released.
- No desktop test exercised `observe`, so there was nothing to port.

**Behaviour change.** A followed call keeps the DND and per-server cue
switches. Before, the shell's follow built the new session without them, so
cues came back on after a move until the preferences changed.

**Files outside Paths:**
- `crates/ss-server/tests/client_core.rs`: the three tests and a `changed`
  helper.
- `apps/desktop/src-tauri/src/stream.rs`: 2 lines, `v.connection` becomes
  `connection()`.
- `lib.rs` is unchanged: `Membership` is reached as
  `ss_client_core::voice::Membership`. rustfmt's `imports_granularity` would
  merge a crate-root re-export into the existing `pub use voice::{…}`,
  which edits that line instead of adding one.

**Open questions:**
- The follow still takes its deafen state from the saved config, as before,
  not from the call. That is the follow rule, which was out of scope.
- If a second move lands while a follow is starting, the follow's
  `JoinVoice` puts the seat back in the first destination, which undoes the
  second move. This was already so. Reading the seat inside `follow`
  would close most of that window.
