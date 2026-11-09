# T-091 — Bound DMs, Welcome size and fan-out so one member cannot lock others out

Status: todo
Phase: 4
Size: M
Depends on: T-028, T-024, T-061
Paths: `crates/ss-server/src/handlers/dm.rs`, `crates/ss-server/src/snapshot.rs`, `crates/ss-server/src/connection.rs`, `crates/ss-server/src/registry.rs`, `crates/ss-server/src/perms.rs`, `crates/ss-server/src/limits.rs`, `docs/security.md`

## Goal
No member can make another member unable to log in, and no ordinary action
makes every session do work proportional to the whole server.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
reproduced or measured.

## Scope
- Group DMs have no cap per creator or per member, and a group DM is
  always created fresh. Every DM and its member ids go into the Welcome
  snapshot, and a Welcome above 1 MiB is rejected with "welcome response is
  too large". At the 20 commands per second quota, about 4,500 group DMs
  (reported estimate, roughly four minutes) lock every targeted user out,
  the owner included, until someone edits the database. Cap group DMs per
  creator and per member, give `CreateDm` its own quota, and never fail
  Welcome on size: page or truncate the DM list and load the rest on
  demand.
- Every `Channel*` event, DM events included, makes every connected
  session recompute its whole channel view (`refreshes_channels`,
  `channel_view`) and triggers a full permission reload, on a four
  connection read pool. Deliver DM channel events only to that DM's
  members, built from the event; keep the full diff for guild role,
  override and channel changes, and debounce it.
- Fan-out: `try_send` on the 32-slot outbound queue discards the frame
  when full and closes the connection only if it is still full ten seconds
  later. The bus is not polled while the session's own command awaits the
  database, so a slow write followed by a burst silently loses events such
  as `TrackAdded` or `MessageCreated`. On a full queue close the connection
  so the client resyncs from Welcome, and run commands off the read loop so
  the bus keeps draining.
- Unconfirmed, needs a large fixture: one command that publishes more than
  256 events (deleting a large occupied category, reordering many roles)
  kicks every other session as a slow consumer.
- Every authenticated HTTP request and token Hello takes the single writer
  to update `sessions`; Hello also takes it in `ensure_defaults`. Batch or
  throttle the `last_seen_at` update.

## Out of scope
Authorization rules (T-090). Attachment transfers (T-092).

## Acceptance criteria
- Test: a member cannot create more group DMs than the cap; a user in the
  maximum number of DMs still logs in.
- Test: a DM rename produces no channel-view recomputation for sessions
  outside that DM (count queries or use a hook).
- Test: a full outbound queue closes the connection instead of dropping a
  frame, and the client's state is complete after it reconnects.
- New limits are listed in `docs/security.md` sections 6 and 7.

## Notes from implementation
