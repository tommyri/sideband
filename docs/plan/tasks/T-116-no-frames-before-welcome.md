# T-116 — Send nothing to a connection before its Welcome

Status: todo
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
