# T-091 — Bound DMs, Welcome size and fan-out so one member cannot lock others out

Status: in-progress
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

Every finding reproduced, including the one the review marked unconfirmed.
Each has a test in `crates/ss-server/tests/dm_fanout.rs` that failed first,
for the stated reason:

| Finding | Test | Before the fix |
|---|---|---|
| Unlimited group DMs lock a member out | `a_member_in_thousands_of_group_dms_still_logs_in` (4,000 seeded group DMs of 10 people, 64-character names) | `Error: welcome response is too large` |
| No caps | `group_dms_are_capped_per_creator_and_per_member` | creation succeeded past both caps |
| No `CreateDm` quota | `opening_new_conversations_has_its_own_quota` | the 11th new conversation succeeded |
| Truncated DMs must surface when active | `an_older_conversation_is_announced_before_its_new_message` | everything was in the Welcome |
| DM rename recomputes every session's view | `a_dm_rename_does_not_recompute_other_sessions_channel_views` (counts `Server::channel_view_count`) | 3 recomputations (actor, member, outsider) |
| Full queue drops frames | `a_full_outbound_queue_closes_the_connection_instead_of_dropping_a_frame`, `a_client_whose_queue_overflowed_is_complete_after_it_reconnects` | frame dropped, connection stayed open; client never learned the change |
| Bus not drained during a command | `a_slow_command_does_not_cost_the_session_its_events` (writer held, 285 presence updates) | actor disconnected as lagged. With 90 updates (below the bus size) it stayed connected and received 33 of 90: silent loss |
| One command overflows the bus | `deleting_a_large_category_keeps_observers_connected` (1,100 children) | observer kicked as a slow consumer (confirmed; was "unconfirmed") |
| Token Hello takes the writer | `a_token_hello_does_not_wait_for_the_writer` | Hello timed out while the writer was held |

`client_core_loads_the_conversations_a_welcome_left_out` covers the client
side of paging.

**Caps and quotas chosen**

- `CreateDm` that would open a conversation: burst 10, then 1 per 6 s, per
  account (`limits::DM_CREATE_*`). Reopening an existing 1:1 DM is answered
  from a read connection, costs no quota and does not take the writer.
- Group DMs started: 100 live per account, counted even after the creator
  leaves, so start-and-leave cannot recycle slots. Group DMs joined: 200 live
  per account; leaving frees a slot, so a victim can always recover by
  leaving. Both apply to `AddDmMembers` too. 1:1 DMs are unique per pair and
  not capped. At the creation quota an attacker needs about ten minutes to
  reach the creator cap, and two accounts to fill a victim's 200.
- `Welcome` carries the 100 most recently active DMs (newest message, else
  creation); `ListDms` pages of at most 100 load the rest. A group DM of ten
  is about 300 bytes on the wire, so 100 is about 30 KB. If a `Welcome` still
  passes 1 MiB the server halves the DM list until it fits.
- Guild channel-view diff debounced to once per 100 ms per session
  (`connection::REFRESH_DEBOUNCE`), flushed early before any event that names
  a channel so a newly visible channel still arrives first.
- `last_seen_at` is re-stamped at most once a minute per token.
- Event bus 256 → 1024, above the 256-connection cap.

**What changed**

- Protocol (new tags only): `ClientEnvelope.list_dms = 45` (`ListDms{cursor?,
  limit}`), `Ack.dm_page = 13` (`DmPage{channels, read_states, next?}`), and
  `Snapshot.direct_messages_next = 8`, an opaque cursor present when the DM
  list was cut short. No `Welcome` field. `02-protocol.md` documents them,
  the DM event rules, and delivery.
- `handlers/dm.rs`: caps, quota, `ListDms`. Create, add and leave patch the
  permission snapshot with `PermissionService::set_direct` instead of a full
  reload; rename no longer reloads (membership is unchanged). DM detection in
  `UpdateChannel` reads the snapshot instead of the database.
- `perms.rs`: roles and grants are shared and each channel view is its own
  `Arc`, so `set_direct` copies pointers. `direct_members` serves member
  lists to the Welcome and `ListDms`.
- `snapshot.rs`: the Welcome lists guild channels only through a new
  `channels::list_guild`, takes DMs from one ordered page query, and limits
  read states to guild channels and the DMs included. `channel_view` is
  guild-only: no membership read, no per-DM `latest_id`. `ensure_defaults`
  checks on a read connection before taking the writer.
- `connection.rs`: each command runs on its own task (aborted if the
  connection drops, as before) while the loop keeps draining the bus;
  commands from one connection stay serial. Replies and bus events wait for
  room in the session's own queue. DM channel events are applied from the
  event by members only (created, updated, or deleted for a session that knew
  it and left); guild changes mark a debounced diff. A DM the session was not
  sent is announced with `ChannelCreated` before its first event. A command's
  guild channel events are published as one trigger. On lag the session now
  waits for the writer to send the notice instead of tearing down first. The
  writer serves a second queue for frames addressed by other tasks, and a
  kick during a stalled write closes the connection (without a notice, since
  the stream is mid-frame) instead of waiting out the 10 s grace.
- `registry.rs`: `enqueue` uses that second queue and closes the connection
  when it is full; the ten-second "blocked since" timer is gone. T-090's
  voice-state gate is unchanged: `enqueue` still calls `admit`, and the
  session's own deliveries call `admit_voice`.
- Client core (additive): `State::direct_messages_next` and
  `Client::list_dms()`, which merges a page without overwriting live copies.

**Files outside the Paths**

`crates/ss-proto/proto/socialspeak/v1/{auth,envelope}.proto` and the
round-trip sample in `ss-proto/tests/frame_round_trip.rs` (one field);
`db/channels.rs` (`list_guild`, `direct_page`, `activity`, `groups_created`,
`groups_joined`); `events.rs` (capacity); `auth/mod.rs` (the `last_seen_at`
throttle in `resume`, about ten lines, which T-093 also edits);
`ss-client-core/src/{commands.rs,state/mod.rs}`;
`tests/authorization_audit.rs` (a `ListDms` policy); `docs/plan/01-architecture.md`
and `02-protocol.md`; the new `tests/dm_fanout.rs`.

**Load measurement for the maintainer** (no VPS here): on the target box with
`ss-cli load`, (1) 200 sessions idle in text while one admin renames a group
DM, then moves 50 roles: compare read-pool wait and CPU against `main`
(expect no per-session work for the DM, one diff per session for the burst);
(2) 4,000 group DMs seeded for one account: time its Hello (expect one page
query, not 4,000 `latest_id` reads); (3) hold a large upload or slow write and
burst chat to 100 sessions: `socialspeak_slow_disconnects` should stay 0 and
no client should miss a message.

**Open questions and follow-ups**

- `channel_members` has no index on `user_id`; the DM page and the joined
  count scan the table. Bounded by the caps, but a migration
  (`CREATE INDEX channel_members_user_id ON channel_members (user_id)`) would
  make both independent of server size.
- A `Welcome` can still pass 1 MiB from members, roles and guild channels
  alone (roughly 8,000+ members). It is logged and refused as before.
- Roles are uncapped; one `MoveRole` over more than 1,024 roles would still
  lag every session.
- T-094: `set_direct` uses `ArcSwap::rcu`, but a concurrent full `reload`
  that read before a DM commit can still overwrite it, the same race T-094
  fixes for reloads. The command task is aborted when its connection drops,
  as the command future was before; T-094 can stop aborting it and move the
  publish into the task. Hello still reloads permissions in full.
- The desktop app does not call `list_dms` yet; older DMs appear as they get
  activity.

