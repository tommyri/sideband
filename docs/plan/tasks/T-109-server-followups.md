# T-109 — Server follow-ups from the review waves

Status: done
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
- Added by the maintainer on 2026-11-11: `auth::login_attempts_are_limited_per_address`
  fails under machine load (it wants `retry_after` of 10–12 s and got
  8.2–9.2 s). Make it independent of wall-clock load without widening the
  window.

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

Each item had a test that failed first for the reason given.

| Item | Test | Before the fix |
|---|---|---|
| Refused join drops seats | `voice_session::a_second_device_refused_by_a_full_channel_leaves_the_first_seat_in_place`, `a_member_refused_by_a_full_channel_stays_in_their_current_one` | `Full`, no leave published, and a fresh `Welcome` showed no seat for the user |
| (guard) seat swap in a full channel | `voice_session::a_member_switches_devices_inside_a_full_channel` | passed; fails if the moving seat is counted against the limit |
| `channel_members(user_id)` index | `dms::dm_lookups_by_member_use_the_user_id_index` (`EXPLAIN QUERY PLAN`) | `SCAN cm` / `SCAN m` |
| Role cap | `permissions::creating_a_role_past_the_cap_is_refused` | the 251st role was created |
| Welcome bound | `welcome_bound::welcome_costs_per_member_role_and_channel`, `the_stated_welcome_examples_fit_one_frame` | (measurement) |
| Webhook order | `client_core::webhook_management_lists_metadata_and_revokes_the_created_url` | 5 of 25 runs failed at `rows[0].id == second.id`, both ids with the same millisecond prefix |
| Auth limiter under load | `auth::login_attempts_are_limited_per_address` | 10 of 10 failed with 28 busy loops on 14 cores (5.6–8.4 s) |

- `Rooms::join` decides every refusal (`Full`, `TracksExhausted`) before it
  moves a seat, so a refused join changes nothing and there is no leave to
  publish. The other device's seat is not counted against the limit, as
  before, because the join removes it. A join that only retunes now also
  returns the other device's leave (it was dropped; unreachable with one
  seat per user, kept for the rule). `peek_track` checks the track space
  without spending an id.
- Migration `0007_channel_members_user_id.sql`: `channel_members (user_id,
  channel_id)`, covering, so the DM page and the group-DM count do not touch
  the table. `03-data-model.md` lists it. The test restates the two queries
  from `db/channels.rs`, which are private. `sqlx::migrate!` has no build
  script here, so an incremental build does not see a new migration until a
  source file under `src/` changes (a clean build does).
- Welcome: measured with `snapshot::build_with` and `prost` encoding, longest
  and typical strings. Longest: 359 B per member, +516 B per online member's
  activity (at most 256), 435 B per role, 18 B per role held, 4,440 B per
  channel with a full topic, 44 B per read state, 138 B per voice seat,
  about 2 KB fixed. Typical: 70 B per member, 43 B per role, 85 B per
  channel. Uncapped roles could fill a frame alone (about 2,400 longest
  roles), and one `MoveRole` over more than 1,024 roles overruns the event
  bus, so roles are capped at 250 counting `@everyone` and Admin
  (`limits::MAX_ROLES`, 109 KB at most): `CreateRole` at the cap fails with
  `FORBIDDEN` "the server already has the maximum number of roles". The
  count runs inside the write transaction. `docs/security.md` §7 states the
  bound: with longest strings and 256 online connections,
  359·M + 435·R + 18·G + 4,484·C ≤ 870,000; with typical strings
  70·M + 43·R + 18·G + 129·C ≤ 1,000,000. Measured examples at the role cap:
  1,000 members holding five roles and 50 channels, all longest, 912,613 B;
  8,000 members holding two roles and 200 channels, all typical, 883,488 B.
  Members, channels and grants stay uncapped; past the bound a `Hello` is
  refused as before. No protocol change.
- Webhook test: compares the first page as a set, checks it is newest id
  first, and pages from whichever row came first. 50 of 50 runs passed.
- Auth test (added scope): the per-address limiter is GCRA on `governor`'s
  own clock, so paused tokio time does not reach it, and an injected clock
  would mean changing `limits.rs` for one test. The test asserts what the
  limiter guarantees instead: the first slot frees one period (12 s) after it
  was spent, so `retry_after ≤ 12 s` and `retry_after + burst time ≥ 12 s`,
  where the burst time is measured around the five attempts (10 ms for the
  two clock reads). A wrong burst or period still fails it. 50 of 50 passed
  with 28 busy loops on 14 cores, and 50 of 50 unloaded.

**Files outside the Paths**
- `crates/ss-server/src/handlers/roles.rs` and `crates/ss-server/src/limits.rs`:
  the role cap.
- `docs/security.md` §7 (table row and the `Welcome` bound),
  `docs/plan/03-data-model.md` (index), this file and `INDEX.md`.

**Open questions and follow-ups**
- `docs/security.md` §12 still lists "A `Welcome` can still pass 1 MiB …;
  roles are not capped. T-109". It sits among T-110's bullets, so it was left
  for the merge: replace it with "Members, channels and role grants are not
  capped; past the bound in section 7 every `Hello` is refused."
- Channels are not capped. With full topics, about 230 channels fill a frame
  on their own; a member with `MANAGE_CHANNELS` can lock every member out.
  A channel cap, or a topic budget, would close it.
- Member paging (a protocol change) is still the only way past about 8,000
  typical members.
