# T-090 — Close authorization gaps: overrides, role edits, voice seats, owner

Status: done
Phase: 4
Size: M
Depends on: T-022, T-025, T-030, T-061
Paths: `crates/ss-server/src/handlers/roles.rs`, `crates/ss-server/src/handlers/channels.rs`, `crates/ss-server/src/handlers/moderation.rs`, `crates/ss-server/src/voice/`, `crates/ss-server/src/perms.rs`, `crates/ss-server/tests/permissions.rs`, `docs/plan/03-data-model.md`, `docs/security.md`

## Goal
Nobody can grant themselves what they do not hold, act on what they cannot
see, or keep hearing a channel they were removed from.

Source: 2026-11-09 review at `d861a0a`, covering what T-061 and T-068
missed. Traced by reading the code, not reproduced. Write the failing
permission tests first.

## Scope
- `set_channel_override` requires only server-wide `MANAGE_CHANNELS` and
  `ensure_can_edit` on the role. There is no `VIEW_CHANNEL` check on the
  target and no limit on the masks. A manager who cannot see a channel can
  allow `VIEW_CHANNEL` for `@everyone` on it and read its history; update,
  delete and move have the same gap. Require `VIEW_CHANNEL` on the target
  for every channel command, as the HTTP override read already does.
- Role edits and override `allow` masks may contain bits the actor does
  not hold; only `ADMINISTRATOR` is withheld. A `MANAGE_ROLES` holder can
  add `MANAGE_SERVER`, `BAN_MEMBERS` and `VIEW_AUDIT_LOG` to `@everyone` or
  to a lower role they hold. Restrict granted bits to those the actor
  holds, per channel for overrides.
- Voice seats: the post-command refresh updates only the speak bit. A
  seated member who loses `VIEW_CHANNEL` or `CONNECT` stays seated and
  keeps receiving every datagram; resume restores a seat without
  rechecking. Evict such seats, emit the leave events, recheck on resume.
  Video already evicts publishers and viewers.
- Owner: `03-data-model.md` says the owner always wins and only
  owner-created roles carry `ADMINISTRATOR`. In code any administrator can
  ban or demote the owner and mint administrator roles, and the seeded
  Admin role can be deleted, after which `user grant-admin` fails on the
  foreign key. Implement the documented rule or change the document; record
  which.
- Leaks around the visibility filter: the actor's reply frames skip
  `delivers`, so a moderator with mute or move rights learns the id of a
  hidden voice channel; `VoiceStateUpdated` with no channel reaches
  everyone; DM create and leave appear in the audit log. Filter all three.
- Banning a user leaves the invites they created valid, and
  `CREATE_INVITES` is in the default role. Revoke them on ban.

## Out of scope
Session revocation (T-093). Abuse limits (T-091, T-092).

## Acceptance criteria
- `tests/permissions.rs` sends non-zero `allow` masks and bit-carrying role
  edits; today it only ever sends `allow: 0`. Each escalation above has a
  test that fails before the fix.
- Integration test: revoke `VIEW_CHANNEL` from a seated member; they are
  removed and receive no further datagrams.
- `tests/authorization_audit.rs` classifies the new checks.
- `docs/security.md` section 5 and `03-data-model.md` match the code.

## Notes from implementation

Every finding reproduced. Each has a test written first and seen failing for
the stated reason (`command_matrix` now reports every mismatched row instead of
stopping at the first, so all fourteen escalations failed in one run):

| Finding | Test (red before the fix) |
|---|---|
| Override on a hidden channel, incl. allowing `VIEW_CHANNEL` to `@everyone` | `permissions.rs` matrix: "manager cannot override a channel they cannot see", "... reveal a hidden channel to @everyone" |
| Channel update, delete, move, create-under on hidden channels | matrix: "cannot rename/delete/move a channel they cannot see", "... into a category they cannot see", "... create a channel in a category they cannot see"; `deleting_a_category_requires_seeing_every_channel_in_it` |
| Override `allow` bits not held (server-wide and per channel) | matrix: "cannot allow a bit they do not hold", "... they are denied in that channel" |
| Role create and edit adding bits not held (`MANAGE_SERVER` on `@everyone`, `VIEW_AUDIT_LOG`, `BAN_MEMBERS`) | matrix rows; `granting_a_role_cannot_hand_out_bits_the_actor_lacks` (assignment) |
| Seeded Admin role deletable and strippable | matrix: "administrator cannot delete / strip ... the seeded Admin role" |
| Non-administrator acting on an administrator | `ACTS` row `(Senior, Admin, false)` |
| Seated member keeps the seat after losing `VIEW_CHANNEL` / `CONNECT` | `revocation.rs`: `losing_view_channel_evicts_a_seated_member_and_stops_their_audio`, `losing_connect_evicts_a_seated_member` |
| Resume restores a seat unchecked | `resume_rechecks_the_seat_it_restores` |
| Moderator's own reply names a hidden voice channel | `a_moderator_never_learns_a_hidden_voice_channel` |
| Channel-less `VoiceStateUpdated` reaches everyone | `leaving_a_hidden_voice_channel_is_not_announced_to_outsiders` |
| DM create and leave in the audit log | `direct_messages_stay_out_of_the_audit_log` |
| Ban leaves the member's invites valid | `banning_a_member_revokes_their_invites` |

Positive rows (a manager allowing or granting bits they hold, an admin
allowing anything, renaming the Admin role, a moved member keeping a seat in a
channel they cannot `CONNECT` to) passed before and after.

**What changed**

- `handlers/roles.rs`: `ensure_grantable` — create, edit, and assignment may
  only add bits the actor holds server-wide; bits already on a role may stay,
  removing is not granting. Overrides need `VIEW_CHANNEL` on the channel (hidden
  or direct channels are not found) and newly allowed bits held in that
  channel. `deny` is not limited. The seeded Admin role cannot be deleted or
  lose `ADMINISTRATOR`.
- `handlers/channels.rs`: create, update, move, and delete need `VIEW_CHANNEL`
  on the target and on a new parent (not found otherwise); a category delete
  is forbidden when any descendant is hidden from the actor.
- `perms.rs`: `can_act_on` is false for a non-administrator acting on an
  administrator, whatever the positions. New `holds` helper.
- `voice/`: `refresh_speak` became `refresh_seats`, which evicts seats that
  lost `VIEW_CHANNEL`, or `CONNECT` unless the seat was placed by `MoveUser`
  (a new `Placement` flag on the seat), and returns the leave events. Resume
  rechecks the leased seat and drops it (publishing the leave) when it may not
  be held; a stale leased peer is detached instead of leaking. `move_member`
  needs the actor to see the destination (else not found) and the target's
  current channel (else "not in a voice channel").
- `handlers/moderation.rs` + `db/invites.rs`: a ban, from the command or
  `ban_account`, revokes the banned user's open invites in the same
  transaction (`revoke_by_creator`) and emits `InviteRevoked`.
- Leaks: `connection.rs` passes the actor's reply event frames through
  `snapshot::delivers` (own invite events excepted, so a creator without
  `MANAGE_SERVER` still sees `InviteCreated`). `registry.rs` remembers per
  connection which voice connections it was told are in a channel (seeded from
  the Welcome snapshot); a leave is sent only to connections that saw the
  arrival. It is applied in `enqueue`, so fan-out and the direct `MoveUser`
  frames both go through it. `db/audit_log.rs` hides `dm.*` rows from the
  audit-log page; the rows stay in the table.
- `tests/authorization_audit.rs`: channel commands and overrides now require
  `VIEW_CHANNEL`; role commands require `ensure_grantable`/`ADMIN_ROLE_ID`;
  ban requires `revoke_by_creator(`; move requires `move_member(actor,`. A new
  `visibility_follows_every_outbound_path` pins the non-command paths (reply
  filter, voice gate in `enqueue`, seat refresh and resume, move view checks,
  CLI ban, the `dm.%` filter).
- Docs: `03-data-model.md` (permission table, hierarchy, granting, visibility,
  voice seats), `docs/security.md` section 5, and one paragraph in
  `02-protocol.md` (leave delivery and eviction are client-visible).

**Decision: owner.** I changed the document rather than adding an owner.
Administrators are peers; the first user is one of them; the operator CLI
(`user grant-admin`, `user unban`) is the recovery path, and the seeded Admin
role is now protected so `grant-admin` cannot hit the foreign-key failure. Two
rules from the old "owner always wins" text are kept in a form the code can
honour: only an administrator can grant `ADMINISTRATOR` (it already held), and
a non-administrator can never act on an administrator, even from a role placed
above Admin (new; before, `can_act_on(Senior, Admin)` was true). An owner
column would need a migration and a CLI story for transferring ownership; it
can be added later on top of this.

**Deviations from Paths.** The leak fixes could not stay inside the listed
paths: `connection.rs` (reply filter, resume publishing), `registry.rs`
(voice-state gate), `db/audit_log.rs` (DM filter), `db/invites.rs`
(`revoke_by_creator`), `docs/plan/02-protocol.md`, and the new
`tests/revocation.rs` (an in-process server suite; kept out of
`voice_session.rs`, which T-081 edits). `tests/authorization_audit.rs` is named
by the acceptance criteria.

**Testing.** `just check` (nightly rustfmt, clippy `-D warnings`, workspace
tests). `client_core::webhook_management_lists_metadata_and_revokes_the_created_url`
fails intermittently (about one run in four or five) on this branch and on the
unmodified base: two webhooks created in the same millisecond get ULIDs whose
random part sorts either way. Not touched here.

**Follow-ups.**

- A Hello reloads the permission snapshot (`snapshot::build_with`) without a
  seat refresh, so a change made outside the process (CLI, direct DB edit)
  reaches seated members only on the next command. The resume path rechecks;
  other seats do not until then.
- Fix or loosen the same-millisecond ordering in the webhook list test.
