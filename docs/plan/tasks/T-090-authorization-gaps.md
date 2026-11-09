# T-090 — Close authorization gaps: overrides, role edits, voice seats, owner

Status: todo
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
