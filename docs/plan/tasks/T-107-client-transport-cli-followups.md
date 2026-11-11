# T-107 — Client transport and CLI follow-ups from the review waves

Status: todo
Phase: 4
Size: M
Depends on: T-089, T-101
Paths: `crates/ss-net/src/control.rs`, `crates/ss-client-core/src/connection.rs`, `crates/ss-client-core/src/history.rs`, `crates/ss-cli/src/`

## Goal
The defects that wave 1 to 4 tasks found outside their own scope, and left
untracked, are fixed or explicitly dropped.

Source: T-099, from the "Follow-ups" of T-081, T-082, T-086, T-089 and
T-101, each checked against the code at `0d0cb16`.

## Scope
- `ss_net::control` gives events an `mpsc` channel of 256
  (`ClientControl::start`) and `read_responses` closes the connection with
  `ControlError::Capacity` when it is full (`try_send`). A burst of more
  than 256 events while the connection loop is busy (a large category
  delete, a role reorder) drops the connection instead of applying
  backpressure. Make the router wait for room, or size and document the
  bound (T-081, T-101).
- `ss-cli` `call.rs` `note_track` finds its own track by `user_id`, so a
  second session of the same account mistakes the other's track for its own.
  Match `conn_id` as the client core does (T-086).
- `ss-cli` `graph.rs` still sends captured audio in the closing quantum after
  the gate closes, which T-082 fixed in `ss-client-core` (T-082).
- History resync after a reconnect ignores `Retry-After` on `429`, and
  mention counts may be counted twice between the bus subscription and the
  snapshot; the four-page budget may move the scroll position. Write the
  failing test first for each, or record why it cannot happen (T-089).

## Out of scope
Desktop shell changes (T-108). New reconnect policy.

## Acceptance criteria
- A test sends more than 256 events while a command is pending; the
  connection stays up or the documented bound is asserted.
- A test runs two CLI sessions of one account in one channel; each finds its
  own track.
- Each resync item has a test or a recorded reason it was dropped.

## Notes from implementation
