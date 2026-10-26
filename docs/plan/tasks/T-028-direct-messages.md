# T-028 — Direct messages and group DMs

Status: todo
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
