# T-094 — Server event correctness: member events, deleted content, cache races, media lock

Status: done
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

Implemented 2026-11-11. Each finding has a test written first and seen
failing for the stated reason, except where noted.

| Finding | Test | Before the fix |
|---|---|---|
| `MemberJoined` never sent | `tests/event_correctness.rs`: `a_registration_is_announced_to_connected_members` | Ada's first frame about Bea was `PresenceUpdated`; no `MemberJoined` |
| `MemberLeft` never sent | none; see below | refuted as stated |
| Deleted message republished by the preview worker | `previews::worker::tests::a_message_deleted_before_its_preview_resolves_is_not_republished` | `MessageUpdated` with the full deleted content and a link row |
| Older reload overwrites a newer one | `perms::tests::an_older_reload_never_replaces_a_newer_one` (deterministic interleaving); `concurrent_hello_and_set_user_roles_end_with_the_new_permissions` (20 rounds of a token Hello racing a demotion) | unit test: the demoted member held the role's bit again. The integration test passed before the fix too: the window is too short to hit by timing, so the unit test is the evidence |
| Connection closed after commit: no reload, no event | `closing_the_connection_after_commit_still_reloads_and_publishes` | "the committed role change was never published" |
| Lease timer evicts a second lease | `voice::session::tests::a_first_lease_timer_does_not_end_a_second_lease` | the first timer's check (`lease.conn == conn`) ended the second lease |

**Permission cache: partly fixed before this task.** T-091's patch log
already stopped a reload from overwriting a newer *DM patch*, and 7a4a151
releases a reload cancelled mid-read. A full reload against a full reload was
still unordered, as the new unit test showed. Each reload now takes a ticket
in `begin_reload`; `finish_reload` drops its read when a reload with a later
ticket has already stored. A reload that began after a commit reads that
commit, so the cache always ends at least as new as the last post-commit
reload. Still no lock across the database read.

**`MemberLeft`: refuted as stated.** No path in the running server disables
or removes an account: `users::set_disabled` and `users::delete` are called
only by tests (`git grep`), and the CLI has no disable or delete command. A ban
keeps the account in the member list: the `Welcome` lists every account
(`users::list`, disabled and banned included), and the moderation pages show
banned members. Sending `MemberLeft` on ban would disagree with the next
`Welcome`. `02-protocol.md` now says `MemberLeft` is reserved. Whether a ban
should remove the member from the list (and old messages fall back to
"Unknown author") is a product decision, left open.

**What changed**

- `connection.rs`: a registration Hello publishes `MemberJoined`
  (`announce_member`, the `Member` built by `handlers::member`, shared with
  `UpdateProfile`) right after the commit and before the presence event; the
  new session subscribes later and does not get its own. The command task is
  no longer aborted when its connection closes, and it publishes its own
  events (`publish` moved from `finish` into the task), so commit, reload and
  publish complete once the command has started; only the reply to the
  closed connection is lost. Others may now see a command's events just
  before the actor's own reply. `refresh_seats` runs only when the command's
  events include a role, grant, override or guild channel change
  (`refreshes_guild`), and after every `Hello`'s permission reload, which
  keeps T-090's fallback for changes made by the operator CLI (previously any
  later command rechecked seats). `Server::session` did not grow.
- `perms.rs`: reload tickets, above.
- `previews/worker.rs`: the check for `deleted_at`, the links and the publish
  happen under the writer, so a delete cannot commit between the check and
  the event, and its `MessageDeleted` always follows the update.
- `voice/session.rs`: lease bookkeeping is a small `Leases` type; each grant
  has its own number and a timer ends only the grant it was started for.
- Media lock: `Rooms` is behind a `parking_lot::RwLock` (was `Mutex`) shared
  by `Voice`, `media::Forwarder` and the video relay. Forwarding, the Welcome
  voice snapshot, stats and lookups read; membership, flags, video commands
  and seat rechecks write. The forwarder still calls `send_datagram` under
  the read lock: readers no longer exclude each other, and writers are rare
  now that chat does not recheck seats. Per-channel recipient snapshots were
  not needed at this scale; they remain the next step if writers ever show up
  in the datagram tail. `01-architecture.md` describes this.
- `handlers/profile.rs`: `member()` builder. `tests/authorization_audit.rs`:
  the outbound table now pins that `replies` gates `refresh_seats(` on
  `refreshes_guild` and that `prepare_welcome` reaches `refresh_seats(`.

**Load measurement (local only; no VPS exists).** T-062 harness, release
builds, `ss-cli load 127.0.0.1:14443 --bots 25 --channels 3 --duty 0.3
--burst 3 --duration 120` against `ss-server run` on the development Mac,
runs interleaved before/after. `--text-rate 60` asks for one message per bot
per second; the server's send quota (burst 5, one per 5 s) accepts roughly
6–16 messages/s in total, and each accepted send used to recheck all 25
seats under the media lock. Relay delay is bot → server → bot. Other
worktrees were compiling: the load average was 6–49 during the runs, which
dominates the tails.

| Run | Threads | Chat | Relay p50 / p99 / max | Text acked | Forwarded | Drops |
|---|---|---|---|---|---|---|
| before | 1 | none | 0.24 / 9.2 / 37 ms | – | 353,730 | 0 |
| after | 1 | none | 0.32 / 10.2 / 74 ms | – | 353,841 | 0 |
| before | 1 | 60/min | 0.38 / 18.4 / 107 ms | 696 | 324,697 | 0 |
| before (repeat) | 1 | 60/min | 0.48 / 10.2 / 41 ms | 1,360 | 332,675 | 0 |
| after | 1 | 60/min | 0.17 / 0.83 / 25 ms | 696 | 327,214 | 0 |
| after (repeat) | 1 | 60/min | 0.35 / 12.3 / 82 ms | 1,971 | 330,462 | 0 |
| before | 4 | 60/min | 0.26 / 2.0 / 23 ms | 1,979 | 329,320 | 0 |
| after | 4 | 60/min | 0.22 / 3.3 / 37 ms | 1,978 | 327,687 | 0 |

No drops, gaps or forced disconnects in any run. At 25 users the difference
is inside the run-to-run noise of this busy machine (the two "before" chat
runs alone differ by 2× at p99). The change removes work from the chat path
(25 permission resolutions under the media lock per accepted message) rather
than showing a measurable latency gain here. The VPS run with 100 users
(T-069) is where to look again; the 32-sessions-per-address limit (`limits::SESSIONS_PER_IP`)
now caps one load process at 32 bots.

**Files outside the Paths:** `media/streams.rs` and `media/streams/relay.rs`
(lock type, `read`/`write`), `voice/rooms.rs` (doc line),
`handlers/mod.rs` (re-export), `tests/event_correctness.rs` (new),
`tests/authorization_audit.rs` (two outbound rows), `docs/plan/02-protocol.md`
(one table cell). None of T-092's files. No protocol change, no client change:
`ss-client-core` already applies `MemberJoined` and `MemberLeft`.

**Open questions and follow-ups**

- Should a ban (or a future disable) remove the member from the `Welcome`
  and send `MemberLeft`? Needs a decision on how messages by former members
  are shown.
- A command now outlives its connection. A detached `JoinVoice` that lands
  after the connection's voice cleanup gets "unknown connection" and does
  nothing; a slow command keeps its task until it finishes.
- The registration announcement and the Hello seat recheck run on the
  session future, which is not cancelled by a connection close; only server
  shutdown drops it.
