# T-028 — Direct messages and group DMs

Status: done
Phase: 2
Size: S
Depends on: T-026
Paths: `crates/ss-server/src/handlers/dm.rs`

## Goal
1:1 and small-group private conversations using the message pipeline as-is.

## Scope
- `CreateDm{user_ids}`: for 1 other user returns the existing `dm` channel
  if present (unique per unordered pair), else creates one; for 2–9 others
  creates a `group_dm` with the caller as creator. Members stored in
  `channel_members`.
- Permission rule: DM channels resolve to `VIEW|SEND|ATTACH` for members
  only (already in `Permissions::resolve`); roles/overrides don't apply.
- Snapshot includes the user's DM channels with last message id and unread
  state; `ChannelCreated` is sent to all members on creation.
- Group DM: `UpdateChannel` name by any member; leaving = `LeaveDm` (add to
  proto); creator can add members (`AddDmMembers`, add to proto).
- Users with `disabled_at` or banned can't be DM'd.

## Out of scope
E2EE, blocking users (backlog).

## Acceptance criteria
- Tests: DM dedupe, non-member cannot read/send, group add/leave, snapshot
  contains DMs, history REST works for DMs with the same endpoint.

## Notes from implementation

- One other user reuses the live 1:1 row for that unordered pair. Two to nine
  others always insert a new `group_dm`. The caller is `created_by` and a
  member. `CreateDm` acks `DmChannel`. Creation emits `ChannelCreated` to
  every member; reuse does not.
- Disabled accounts and active bans are `FORBIDDEN` "user cannot be
  messaged". Unknown ids are `NOT_FOUND`. A caller listed among `user_ids`
  is ignored. Empty after that, or more than nine others, is `INVALID`.
- `LeaveDm` is group-only. `AddDmMembers` is the creator only, and the room
  stays at most ten people. A creator who has left is no longer a member, so
  the add is `NOT_FOUND`. Any member may `UpdateChannel` the group name;
  other patch fields are rejected. Guild channel commands are unchanged.
- The snapshot lists a member's DMs with `last_message_id` (newest visible
  message) and the existing read-state row. History uses the same
  `/api/v1/channels/{id}/messages` route. Non-members, including
  administrators, get the hidden-channel failure.
- `Welcome.features` includes `dms`.
