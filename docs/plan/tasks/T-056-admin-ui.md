# T-056 — Admin UI: server settings, roles, members, invites, channels, audit log

Status: in-progress
Phase: 3
Size: M
Depends on: T-042, T-030, T-023, T-031
Paths: `apps/desktop/src/features/admin/`

## Goal
Everything an admin needs without touching the CLI.

Completed increments: [T-056A](T-056A-invite-management.md) delivers invite management;
[T-056B](T-056B-audit-log.md) adds the filterable audit-log viewer;
[T-056C](T-056C-webhook-management.md) adds webhook management.
The parent remains open for server settings, roles, members/moderation, channel
overrides, including permission-model acceptance.

## Scope
- Server settings: name, description, icon (attachment), registration
  mode, TLS fingerprint display, server version.
- Roles editor: list with drag-reorder (hierarchy), create/rename/color,
  permission checkboxes grouped (General / Members / Text / Voice) with
  descriptions, member assignment; disabled states for things above your
  rank.
- Members: table with search, roles chips, join date, last seen; actions
  kick/ban (reason, duration presets), unban list.
- Invites: create (uses/expiry), copy, revoke, see creator/uses.
- Channel editor dialogs (create/edit): name, topic, kind, parent, voice
  profile + bitrate slider within range, user limit; per-channel role
  overrides table (allow/deny/inherit tri-state).
- Webhooks: create (channel, name), reveal-once URL, delete.
- Audit log viewer with filters (actor, action, date) using the REST
  endpoint.

## Out of scope
Server-side config file editing.

## Acceptance criteria
- Every action is permission-gated in UI and verified against server
  denials (shows error toast rather than crashing).
- Vitest tests for the permission checkbox model (dependencies like
  `SPEAK` implies `CONNECT` hint).

## Notes from implementation
