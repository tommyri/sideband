# T-116 — Send nothing to a connection before its Welcome

Status: done
Phase: 4
Size: S
Depends on: T-113
Paths: `crates/ss-server/src/connection.rs`, `crates/ss-server/src/registry.rs`, `crates/ss-server/tests/`

## Goal
The first frame a client receives after `Hello` is the reply to it.

Source: T-113's open questions, read from the code at the T-113 commit.

## Scope
- Frames other tasks address to one connection through
  `registry::enqueue` (a `ReadStateUpdated` from the user's other session,
  `SubscriberHint`, `MoveUser`) can be written before the `Welcome`: the
  connection is registered before the snapshot is built, and
  `write_outbound` serves that queue as soon as the pump starts.
- The client's `establish` waits only for the `Hello` reply (T-107), so an
  earlier frame may fail the sign-in or be applied to state the `Welcome`
  then replaces. Check what the client does today with a test.
- Hold the addressed queue until the `Welcome` has been written (or
  register only after it), keeping the T-091 queue bounds and T-113's
  sign-in ordering.

## Out of scope
The shared event bus, which T-113 already orders around the snapshot.

## Acceptance criteria
- Test: a `ReadStateUpdated` from the user's other session, sent while a
  second session signs in, reaches that session after its `Welcome`, and the
  sign-in succeeds.

## Notes from implementation

Implemented 2026-11-12. No protocol change.

| Test | Before the fix | After |
|---|---|---|
| `sign_in_order::a_read_state_from_another_session_reaches_a_signing_in_session_after_its_welcome` (acceptance) | first frame after `Hello` was the other session's `ReadStateUpdated` | `Welcome` first, then the read state; a later `Ping` is answered |
| `sign_in_order::a_kick_aimed_at_a_signing_in_connection_follows_its_welcome` | first frame was `Kicked`, no `Welcome` | `Welcome`, `Kicked`, close code 3 |
| `sign_in_order::a_session_revoked_while_it_signs_in_is_closed_after_its_welcome` (operator revoke, then `Sessions::sweep`) | first frame was `SessionRevoked`, no `Welcome` | `Welcome`, `SessionRevoked`, close code 6 |
| `client_core::a_sign_in_behind_hundreds_of_addressed_frames_still_succeeds` (300 enqueued `ReadStateUpdated`, then a real `MarkRead`) | `Client::connect` failed with `Timeout` after 10 s | signs in; ends at the newest read state |
| `client_core::a_member_kicked_while_signing_in_is_removed` | `Client::connect` failed with `Control(Read(ConnectionLost(ApplicationClosed { error_code: 3 })))`; the `Kicked` frame was never seen | signs in, then is removed and does not reconnect |

Each test holds the sign-in with `Server::hold_sign_ins()`, a
`#[doc(hidden)]` hook that takes T-113's bus hold. A sign-in waits on it in
`quiet()`, after `registry.register` and before the snapshot's read point,
so the frame is queued while the connection is registered and has no
`Welcome` yet. The tests wait for the new connection id in
`registry().connection_ids(user)` before acting.

**What the client did with an early frame.** It neither failed nor ignored
one frame: the `ss-net` router puts an id-0 frame in the event queue, and
`establish` reads only the `Hello` reply, so the frame waited there and the
runner applied it after `StateStore::welcome`, on top of the snapshot. Two
real failures followed from that:
- More early frames than the router's event queue (`EVENT_QUEUE`, 256): the
  router waits for room, nothing drains it during `establish`, and the
  `Welcome` behind them never arrives. The sign-in timed out. The direct
  queue accepts up to `DIRECT_LIMIT` (1024), so this was reachable.
- A kick or revocation during sign-in: the writer served its kick slot
  before anything, sent the notice and closed without a `Welcome`. The
  client saw only a lost connection on its `Hello`. On the first connect it
  returned a connection error instead of "kicked"; on a reconnect that error
  is not terminal, so after a kick the client would simply sign in again.

**Fix** (`connection.rs`). `write_outbound` became `Outbound`, whose `run`
takes the `Welcome` and writes it before it looks at the kick slot, the
direct queue or the session's own queue. `read_inbound` no longer sends the
`Welcome`; `pump` builds the `View` and seeds the voice gate from it before
starting both halves. The loop body was split into `next` (kick first, then
the direct queue, then the own queue, as before) and `write` (one frame,
with the slow-consumer timeout and the mid-write kick handling unchanged),
so no function grew past clippy's limit and `Server::session` is unchanged.

- **T-091 bounds.** Frames addressed during sign-in wait in the same
  `DirectQueue`, with its grace and `DIRECT_LIMIT`. The wait is the snapshot
  build plus one `Welcome` write. Registration still precedes the snapshot,
  so nothing addressed during sign-in is lost.
- **T-113 ordering.** Untouched: registration, then the snapshot and the bus
  subscription under `quiet()`, then the `Welcome`. The bus events go through
  the session's own queue, which only `read_inbound` fills, after the
  `Welcome` now as before.
- **Kicks and revocations during sign-in.** Both go to the kick slot, which
  exists from `Sessions::attach` and `registry.register` on (before the
  snapshot). A pending kick now waits for the `Welcome` write: `write`
  selects on the kick slot during the write, so it lets the `Welcome`
  finish for up to `NOTICE_TIMEOUT` (1 s), then sends the notice and closes
  with the kick's code; if the `Welcome` does not finish in that time the
  connection is closed with the code and no notice. A second kick while one
  is pending closes the QUIC connection at once (`kick_peer`'s `try_send`
  fallback), as before. So a kicked, banned or revoked connection is still
  closed, now after a `Welcome` it was entitled to when it authenticated,
  and the client reports it as removed.

**`enqueue` callers checked.** `publish` (`ReadStateUpdated` to the user's
other sessions), `moderation::move_user` (`MoveUser`'s direct events to a
connection that resumed a leased seat), `media/streams.rs`
(`SubscriberHint`, `RequestKeyframe`; a signing-in connection has no
tracks), and the media rate-limit notice in `pump` (only once the pump
runs). All now follow the `Welcome`. Kicks, bans, revocations, slow-consumer
kicks and shutdown use the kick slot, above. A move before `voice.fill`
shows in the snapshot and its events then repeat it; one after is applied
from the events.

**Docs.** `02-protocol.md` §1 item 3 now says the reply to `Hello` is the
first frame and what arrives during sign-in follows it.

**20 runs.** After `cargo test -p ss-server --test sign_in_order --test
client_core --no-run`: `for i in $(seq 1 20); do cargo test -p ss-server
--test sign_in_order || echo "run $i failed"; cargo test -p ss-server --test
client_core -- a_sign_in_behind a_member_kicked_while || echo "run $i
failed"; done` printed no failure: 20 times "3 passed" and 20 times
"2 passed".

**Files outside the Paths**
- `docs/plan/02-protocol.md`: one sentence in §1.
- `docs/plan/tasks/INDEX.md`: status.

Nothing in `auth/` or the `Hello` credential branch (T-080), nor
`config.rs` or the client (T-115). No `.proto` or client change.

**Tests.** `just check` passes: nightly rustfmt, clippy `-D warnings`, the
workspace tests (746 passed, 6 ignored, in 59 binaries) and `just
plan-check`. `cargo clippy -p socialspeak-desktop --all-targets -- -D
warnings` passes.

**Open questions**
- A `ReadStateUpdated` carries an absolute `mention_count` computed at the
  `MarkRead`. One enqueued before a signing-in session's read point is
  already in its snapshot; delivered after the `Welcome`, it overwrites the
  snapshot's count with an older one if a mention was committed in between.
  The same race exists for live sessions, since the read state travels on
  the direct queue and `MessageCreated` on the bus. Fixing it needs the
  enqueue ordered with the message hold (or a horizon in the event), in
  `handlers/messages.rs`; not done here.
- A ban or kick committed after `Hello` passed the ban check and before
  `registry.register` finds no connection to close. A revocation in that
  window is caught (`Sessions::attach` comes first, and the sweep runs every
  30 s); a kick or ban is not. Pre-existing.
- The client still keeps reading nothing but the `Hello` reply during
  `establish`, which is now safe because the server sends nothing before it.
