# T-094 — Server event correctness: member events, deleted content, cache races, media lock

Status: todo
Phase: 4
Size: M
Depends on: T-024, T-025, T-026, T-029, T-091
Paths: `crates/ss-server/src/previews/worker.rs`, `crates/ss-server/src/handlers/`, `crates/ss-server/src/perms.rs`, `crates/ss-server/src/connection.rs`, `crates/ss-server/src/media/permitted.rs`, `crates/ss-server/src/voice/session.rs`, `crates/ss-server/src/auth/`, `docs/plan/01-architecture.md`

## Goal
What the server tells clients matches what is in the database, and the
media path does not wait on unrelated work.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
reproduced or measured. Touches `connection.rs`; run after T-091.

## Scope
- `MemberJoined` and `MemberLeft` are in the protocol and never emitted
  anywhere in the server. Connected clients learn of a new registration
  only when they reconnect. Emit them on registration and on account
  disable or removal.
- The link-preview worker loads the message with a query that includes
  soft-deleted rows, and a soft delete keeps `content`. A message deleted
  two seconds after posting is published again in full as `MessageUpdated`
  when the fetch finishes. Skip publishing and linking when `deleted_at` is
  set.
- Permission cache: reload is an unsynchronised load-then-store, and every
  Hello reloads outside the write lock, so an older snapshot can overwrite
  a newer one and a demoted user keeps old permissions until the next
  reload. Serialise reloads with a generation counter or reload under the
  write lock everywhere.
- The command future is dropped at any await when its connection closes.
  If that happens between `tx.commit()` and `perms.reload()` or the
  publish, the change is committed with no reload and no event. Shield or
  spawn the commit–reload–publish section so it completes once started.
- Media hot path: one global `Mutex<Rooms>` is held across the whole
  fan-out including each `send_datagram`; `refresh_speak` runs after every
  successful non-voice command by anyone and resolves permissions for every
  seated peer under that lock. `01-architecture.md` specifies a read-heavy
  `RwLock`. Use one, or snapshot per-channel recipient lists; run the
  refresh only after role, override and channel mutations.
- Voice resume lease: the first 60 s timer matches on connection id only,
  so disconnect, resume, disconnect again lets the first timer evict the
  second lease early.

## Out of scope
Authorization rules (T-090). Fan-out queue policy (T-091).

## Acceptance criteria
- Tests: a second client sees `MemberJoined` when a user registers; a
  message deleted before its preview resolves produces no `MessageUpdated`;
  concurrent Hello and `SetUserRoles` always end with the new permissions.
- Test: closing the admin connection right after commit still reloads
  permissions and publishes the event.
- Load-test note: datagram forwarding latency with chat traffic before and
  after the lock change, using the T-062 harness.
- `01-architecture.md` describes the lock that the code uses.

## Notes from implementation
