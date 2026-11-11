# T-109 — Server follow-ups from the review waves

Status: todo
Phase: 4
Size: S
Depends on: T-094
Paths: `crates/ss-server/src/voice/rooms.rs`, `crates/ss-server/migrations/`, `crates/ss-server/src/snapshot.rs`, `crates/ss-server/tests/`

## Goal
The server defects that T-086, T-090 and T-091 noted outside their scope
are fixed or recorded as accepted limits.

Source: T-099, checked against the code at `0d0cb16`.

## Scope
- `Rooms::join` removes the user's seat on another connection, and the
  joining connection's old seat, before it checks the channel's user limit.
  A join that then fails with `Full` has already removed those seats, and
  their leave events are returned only on success, so they are never
  published (T-086). Check the limit first, or publish the leaves on error.
- Add a `channel_members(user_id)` index in a new migration; DM lookups by
  member scan the table (T-091).
- A `Welcome` can still pass 1 MiB from members, roles and guild channels
  (about 8,000 members), and roles are uncapped (T-091). Cap roles, or page
  members, or state the limit in `docs/security.md` §7.
- `client_core::webhook_management_lists_metadata_and_revokes_the_created_url`
  fails about one run in five: two webhooks made in the same millisecond get
  ULIDs whose random part sorts either way (T-090). Make the test independent
  of that order.

## Out of scope
Member paging in the protocol, unless the third item needs it; then it is a
protocol change and updates `02-protocol.md`.

## Acceptance criteria
- A test fills a channel, then joins it from a seated user's second device:
  the join fails and the first seat is still there (or its leave is
  published).
- The webhook test passes 50 runs in a row.
- `docs/security.md` states the Welcome bound that holds.

## Notes from implementation
