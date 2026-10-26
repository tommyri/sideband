# T-030 — Moderation: kick, ban/unban, server mute, move, audit log

Status: todo
Phase: 2
Size: M
Depends on: T-022, T-024, T-025
Paths: `crates/ss-server/src/handlers/moderation.rs`, `crates/ss-server/src/http/audit.rs`

## Goal
Admins and trusted members can enforce rules, and every privileged action is
recorded.

## Scope
- `KickUser` (`KICK_MEMBERS` + hierarchy): send `Kicked{reason}`, close all
  of the target's connections, remove from voice; audit.
- `BanUser` (`BAN_MEMBERS` + hierarchy): insert ban with optional expiry,
  then kick with `Banned`; registration with the same username blocked while
  banned; `UnbanUser` sets `lifted_at`; expired bans ignored at `Hello`.
- `ServerMuteUser` (`MUTE_MEMBERS`): toggles router flag, emits
  `VoiceStateUpdated`; persists across reconnects for the session (in-memory
  map keyed by user; cleared on server restart; document).
- `MoveUser` (`MOVE_MEMBERS`): target must be in voice; destination checks
  as in `JoinVoice` but bypassing `user_limit`.
- `RevokeInvite`, `CreateInvite` (`CREATE_INVITES`; `MANAGE_SERVER` to list
  all).
- Audit log writes for all of the above plus role/channel mutations (hook
  from T-022/T-023); `GET /api/v1/audit-log` with `VIEW_AUDIT_LOG`.
- CLI equivalents for recovery: `ss-server user ban|unban|set-password`,
  `ss-server role grant-admin <user>`.

## Out of scope
Timeouts/slowmode (backlog), word filters.

## Acceptance criteria
- Tests: hierarchy denial (equal/greater target), kick closes connection and
  voice, ban blocks reconnect until expiry, unban allows, move respects
  permissions, audit rows written with correct actor/target/details.

## Notes from implementation
