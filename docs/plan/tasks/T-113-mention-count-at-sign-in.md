# T-113 — Count a mention once when it arrives during sign-in

Status: todo
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
