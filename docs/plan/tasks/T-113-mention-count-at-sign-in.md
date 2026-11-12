# T-113 — Count a mention once when it arrives during sign-in

Status: done
Phase: 4
Size: S
Depends on: T-107, T-109
Paths: `crates/ss-server/src/connection.rs`, `crates/ss-server/src/snapshot.rs`, `crates/ss-server/tests/client_core.rs`, `docs/plan/02-protocol.md`

## Goal
The mention badge shows the true count after signing in, even when
mentions arrive while the session is being set up.

Source: T-107, which reproduced it. The ignored test
`client_core::mentions_stored_while_a_session_signs_in_are_counted_once`
signs in twelve sessions while four accounts send 20 mentions, and every
run counts 21 to 24.

## Scope
- The server subscribes the session to the event bus before it builds the
  Welcome snapshot. A mention stored in between is counted in the
  snapshot's read state and again when its live `MessageCreated` arrives.
  The client cannot tell them apart: guild channels have no
  `last_message_id` in the snapshot, there is no read-state query, and ULIDs
  are not monotonic.
- Either build the snapshot's read states and publish `MessageCreated`
  under one lock so each mention lands on exactly one side, or add a message
  horizon to the snapshot so the client can drop events already counted.
  The second is a protocol change: update `02-protocol.md` and add only new
  fields.
- Keep the T-091 fan-out limits and T-094's command behaviour intact.

## Out of scope
Client-side counting changes beyond using a new horizon field.

## Acceptance criteria
- The ignored test is enabled and passes 20 runs in a row.

## Notes from implementation

Implemented 2026-11-12. No protocol change: the "one lock" option.

| Test | Before the fix | After |
|---|---|---|
| `client_core::mentions_stored_while_a_session_signs_in_are_counted_once` (no longer ignored) | `[22, 21, 22, 22, 22, 23, 22, 22, 23, 22, 22, 22]`, the database says 20 | 20 of 20 runs passed |
| `snapshot::tests::a_mention_committed_during_sign_in_is_counted_or_delivered_not_both` (new) | the same steps in the old order (subscribe, commit, build, publish) counted the message and delivered it | counted once, the sign-in waits for the publish |

**Cause.** Two gaps, both in `Server::session`. It subscribed to the bus
before `prepare_welcome`, and `snapshot::build_with` read the read states
statement by statement on a pooled connection. A mention committed after
the subscription and before its `unread_mentions` query was counted and
then delivered. A send also published from the command task after it had
dropped the writer, so a message committed before the subscription could
still be published after it: counted, then delivered, too.

**Fix.** Each message lands wholly on one side of a read point:
- `EventBus` has a `horizon` `tokio::sync::RwLock`. `hold()` (exclusive) is
  taken by a sender that already holds the writer, just before the insert,
  and dropped after `MessageCreated` is published. `quiet()` (shared) is
  taken by a session signing in.
- `snapshot::build_live` reads the whole snapshot in one read transaction.
  After `ensure_defaults` and the permission reload it takes a read
  connection, begins, and under `quiet()` runs one query
  (`fix_read_point`; a deferred SQLite transaction takes its snapshot at
  its first read) and subscribes. Everything it counts was published
  before the subscription; everything committed later is delivered.
  `build_with` calls it with a bus of its own.
- `prepare_welcome` returns the subscription with the `Welcome`; `session`
  no longer subscribes first. Nothing the snapshot shows is read before the
  subscription except the permission reload, and commands update that
  cache before they publish. Presence and voice are read after it.
- `Messages::send` publishes `MessageCreated` itself, with the connection
  as origin, under the hold and the writer, and still returns it so the
  actor's reply is unchanged. `publish` in `connection.rs` skips
  `MessageCreated` for that reason. Webhook posts commit and publish under
  the hold too, though they carry no mentions.
- Edits and deletes are not held. The client changes a count for them only
  for a message it has cached, and at sign-in its cache comes from pages
  fetched after the `Welcome`, so they cannot double count.

**Cost.** One uncontended `RwLock` write acquire per send. The hold covers
the insert, attachments and `to_proto`, which already run under the writer.
A sign-in can wait for one send's hold, and a send for the sign-ins
currently in `quiet()`, which is one query each. At most four, the read pool
size, since the connection is taken before `quiet()`. The writer is never
taken by a sign-in, so T-091's "token Hello does not wait for the writer"
holds: a long write delays only the sends queued behind it. Timed in the
client test (debug build, 3 runs, 63 sends, 54 sign-ins, other worktrees
compiling): a send waited 0.8 µs at p50 and 10 ms at most for the hold; a
sign-in waited 0.19 ms at p50 and 35 ms at most for `quiet()`, which it held
for 0.26 ms at p50. The rest of the snapshot is unchanged, now in one
transaction. `Server::session` got shorter. T-091's fan-out and T-094's
command task are unchanged: the command still runs on its own task, outlives
its connection, and publishes before it answers.

`02-protocol.md` §Delivery states the guarantee: a message counted in a
`Snapshot.read_states` `mention_count` is never sent as `MessageCreated`
after the `Welcome`, and every later message is.

**20 runs.** After `cargo test -p ss-server --test client_core --no-run`:
`for i in $(seq 1 20); do cargo test -p ss-server --test client_core
mentions_stored_while_a_session_signs_in_are_counted_once || echo "run $i
failed"; done` printed no failure and 20 "1 passed" results.

**Files outside the Paths**
- `crates/ss-server/src/events.rs`: `hold`, `quiet` and the lock.
- `crates/ss-server/src/handlers/messages.rs`: `dispatch` takes the origin
  connection; `send` publishes under the hold.
- `crates/ss-server/src/handlers/webhooks.rs`: `execute` publishes under the
  hold, before it drops the writer.
- `docs/plan/tasks/INDEX.md`: status.

None of T-112's files (`limits.rs`, channel handlers, `tests/welcome_bound.rs`,
which still calls `build_with` unchanged). No `.proto` or client change.

**Tests.** `just check` passes: nightly rustfmt, clippy `-D warnings`, the
workspace tests (732 passed, 6 ignored, in 58 binaries) and `just plan-check`.
`cargo clippy -p socialspeak-desktop --all-targets -- -D warnings` passes.

**Open questions**
- `publish` skips every `MessageCreated` in a command reply on the grounds
  that `Messages::send` already published it. A future command that returns
  one without publishing would lose it silently.
- Frames other tasks address to a connection (`registry::enqueue`: a
  `ReadStateUpdated` from the user's other session, `SubscriberHint`,
  `MoveUser`) can be written before the `Welcome`, since `write_outbound`
  serves that queue as soon as the pump starts and registration comes
  first. Not changed here. A `MarkRead` elsewhere during sign-in could reach
  the client before the snapshot replaces it.
- A mention the snapshot counted and that is deleted later is not taken off
  the count, because the client only adjusts counts for messages it has
  cached. That is not specific to sign-in and is left as it was.
- `01-architecture.md`'s event fan-out bullet does not mention the hold. I
  left it alone because T-064 is editing docs at the same time. One sentence
  could be added at merge.

