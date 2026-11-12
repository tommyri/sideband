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
| `sign_in_order::a_member_kicked_while_signing_in_is_refused_without_a_snapshot` | first frame was `Kicked`, no reply to `Hello` | `Error FORBIDDEN` for the `Hello`, `Kicked`, close code 3; no `Welcome` |
| `sign_in_order::a_member_banned_while_signing_in_is_refused_without_a_snapshot` (`BanUser`) | first frame was `Banned`, no reply to `Hello` | `Error BANNED`, `Banned`, close code 4; no `Welcome` |
| `sign_in_order::a_session_revoked_while_it_signs_in_is_refused_without_a_snapshot` (operator revoke, then `Sessions::sweep`) | first frame was `SessionRevoked`, no reply to `Hello` | `Error UNAUTHENTICATED`, `SessionRevoked`, close code 6; no `Welcome` |
| `sign_in_order::a_ban_or_revoke_without_a_kick_is_caught_before_the_welcome` (ban row written, session row deleted, no kick or sweep) | full `Welcome` | refused as above |
| `client_core::a_sign_in_behind_hundreds_of_addressed_frames_still_succeeds` (300 enqueued `ReadStateUpdated`, then a real `MarkRead`) | `Client::connect` failed with `Timeout` after 10 s | signs in; ends at the newest read state |
| `client_core::a_member_kicked_while_signing_in_is_refused` | `Client::connect` failed with `Control(Read(ConnectionLost(ApplicationClosed { error_code: 3 })))`; the `Kicked` frame was never seen | `Client::connect` fails with `Error::Server { FORBIDDEN, "kicked" }` |
| `client_core::a_member_kicked_while_reconnecting_stops_reconnecting` | (passes on both) | `Failed`, and no further `Hello` |
| `connection::tests::a_kick_during_sign_in_refuses_the_hello_with_the_code_it_implies` (unit) | new | close code to `Error` code mapping |

The first-commit rows of the kick and revoke tests are in the review fix
below: that commit sent the `Welcome` and then the notice, which review
rejected.

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
takes the `Welcome` and writes it before the direct queue and the session's
own queue; a kick already in the kick slot replaces it (below). `read_inbound` no longer sends the
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
- **Kicks and revocations during sign-in.** See the review fix: they
  refuse the `Hello`, and no snapshot is sent.

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
first frame, that events arriving during sign-in follow it, and how a kick,
ban or revoke during sign-in refuses the `Hello`.

**Review fix (second commit).** Review found that the first commit wrote
the `Welcome` before looking at the kick slot, so a session kicked, banned
or revoked while it signed in received the whole snapshot (members,
channels, read states, voice) before its notice. Before T-116 the kick slot
was served first and no snapshot went out. Both checks are now done:

- *Pending kick wins.* `Outbound::run` takes the kick slot with `try_recv`
  before writing the `Welcome`. If a kick is there, it sends the `Error` it
  implies for the `Hello`'s request id instead (`hello_refused_by`:
  kicked `FORBIDDEN`, banned `BANNED`, signed out `UNAUTHENTICATED`, which
  §1.2 makes final; shutdown or slow consumer `INTERNAL`, which the client
  retries), then the kick's notice and its close code, as before.
- *Recheck after the snapshot.* `Server::admit_again`, the last step of
  `prepare_welcome`, reads the account's bans and the session row again on
  a read connection (two queries per sign-in). A ban, or a deleted or
  expired session, refuses the `Hello` the same way (`Error`, `Banned` or
  `SessionRevoked`, close 4 or 6). This catches what never reached the kick
  slot: a ban or revoke committed before the connection was registered, and
  a revoke by the operator CLI before the sweep. Kicks leave no row, so a
  kick before registration is still missed. A database error refuses with
  `INTERNAL`.
- *Order.* Registered, snapshot, recheck, kick-slot check, `Welcome`. A ban
  committed before the recheck is caught by it; one after it reaches the kick
  slot, since the connection is registered. Only a kick that arrives while
  the `Welcome` is already being written follows it (notice after the
  `Welcome` write, up to 1 s), the same as a kick just after sign-in.
- *Tests.* `refused` reads every frame until the stream ends and asserts the
  first is the `Hello`'s `Error` and no `Welcome`, `ReadStateUpdated` or
  `PresenceUpdated` comes at all. Run against the first commit's
  `connection.rs`, all four refusal tests got a full `Welcome` first, and
  `a_member_kicked_while_signing_in_is_refused` got `Ok`. In the ban and
  sweep tests the row changes before the recheck, so they go through
  `admit_again`; the kick test goes through the kick slot; the unit test
  covers the slot's mapping for every close code.
- *Client.* Unchanged. A refused first sign-in returns
  `Error::Server { code, .. }` from `Client::connect`. On a reconnect all
  three codes are terminal, so the client stops in `Failed(..)` and does not
  try again. It does not emit `ClientEvent::Removed` for a kick refused at
  `Hello`: the Error carries no "kicked" marker other than its code and
  message, and inferring removal from `FORBIDDEN` was not worth a client
  change here.
- The ban predicate (`lifted_at` unset, expiry in the future) is now written
  three times: `auth::active_ban`, `moderation::active_ban` and
  `admit_again`. I left `auth/` alone because T-080 edits it.

**20 runs.** After `cargo test -p ss-server --test sign_in_order --test
client_core --no-run`: `for i in $(seq 1 20); do cargo test -p ss-server
--test sign_in_order || echo "run $i failed"; cargo test -p ss-server --test
client_core -- a_sign_in_behind a_member_kicked_while || echo "run $i
failed"; done` printed no failure: 20 times "3 passed" and 20 times
"2 passed" for the first commit; after the review fix, 20 times "5 passed"
(`sign_in_order`) and 20 times "3 passed" (the filter now also matches the
reconnect test).

**Files outside the Paths**
- `docs/plan/02-protocol.md`: §1 item 3 (the `Welcome` comes first; how a
  kick, ban or revoke during sign-in refuses the `Hello`).
- `docs/plan/tasks/INDEX.md`: status.

Nothing in `auth/` or the `Hello` credential branch (T-080), nor
`config.rs` or the client (T-115). No `.proto` or client change.

**Tests.** `just check` passes: nightly rustfmt, clippy `-D warnings`, the
workspace tests (746 passed, 6 ignored, in 59 binaries; 750 passed after the
review fix) and `just plan-check`. `cargo clippy -p socialspeak-desktop --all-targets -- -D
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
- A kick that lands after `Hello` and before `registry.register` finds no
  connection to close and leaves no row for the recheck, so that session
  signs in. Bans and revokes in that window are now refused by the recheck.
- A kick refused at `Hello` ends a reconnecting client in `Failed`, not
  `Removed`. If the desktop should say "kicked" there, the client could map
  a `FORBIDDEN` `Hello` refusal to `Removed`; not done.
- The client still keeps reading nothing but the `Hello` reply during
  `establish`, which is now safe because the server sends nothing before it.
