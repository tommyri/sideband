# T-056E — Roles, assignments and channel permissions UI

Status: done
Phase: 3
Size: M
Depends on: T-042, T-022, T-023
Parent: T-056
Paths: `apps/desktop/src/features/admin/`, `apps/desktop/src-tauri/src/role_admin.rs`

## Scope
Role creation, names/colors, grouped permission checkboxes with descriptions,
confirmed deletion, drag ordering and accessible up/down controls. Search members
and replace their role assignments while preserving protected grants. Read and edit
per-role channel overrides with Inherit/Allow/Deny controls and reset to inherit.

## Acceptance
- Separate MANAGE_ROLES and MANAGE_CHANNELS access; protected roles, members and
  administrator grants are disabled. Default membership is always applied.
- Server denials preserve drafts. Observed concurrent changes block stale saves.
  Disconnects discard editors. Deletion requires typing the role name.
- Explain voice/text permission dependencies, administrator bypass and higher-role
  override precedence; categories do not inherit overrides into child channels.
- Load stored overrides before enabling saves; failed reads never become empty
  inherited defaults. Preserve unshown bits and represent masks as decimal strings.
- Real-server tests cover hierarchy, role events, assignments, atomic reordering,
  color clearing and deletion; HTTPS tests cover override auth, visibility, DMs and pins.
- Frontend tests, workspace/desktop checks, production build and browser checks pass.

## Implementation — 2026-11-04
The sidebar opens Roles and permissions. Role managers can change roles and member
assignments; channel managers can read/edit overrides for visible guild channels.
The server remains authoritative for every mutation. Administrators bypass role
hierarchy as they do in the existing permission service. Self-assignment warns
that the change can remove administrative access.

`MoveRole` applies one transaction and emits every changed position. Deletion now
also emits compacted positions. Non-admin moves cannot cross their highest role;
if normalizing equal protected ranks could change authority, an administrator must
resolve that ordering first. `UpdateRole.clear_color` explicitly removes colors.
Both additions are gated by the `role-management-v2` server feature for older-server
compatibility. Existing role operations remain available on older servers.

The authenticated, pinned HTTPS override read requires MANAGE_CHANNELS and
VIEW_CHANNEL and excludes deleted channels and direct conversations. The editor
shows channel-scoped bits; other stored bits are preserved. Both permission words
are u64 decimal strings across the native boundary and BigInt in the frontend.
Conflict detection is client-side observation, not an atomic compare-and-swap.

The parent T-056 remains open for server settings and broader member moderation.
Installed cross-platform acceptance remains manual testing work.

## Validation
`just check` passed; final `just check-desktop` passed with 100 frontend tests and
76 native desktop tests, including the real two-client role bridge. The production
frontend build passed with the existing bundle-size warning.

Chromium verification at 1200×900 and 760×600 covered role editing, denial/retry,
drag and button ordering, member assignment, loading/saving overrides, observed
concurrent edits, typed deletion, focus restoration, Escape and disconnects.
Screenshots were inspected; content, console and framework-overlay checks passed.
The browser used a mocked native bridge; separate integration tests used a real
local QUIC server and pinned HTTPS listener. No GitHub Actions or releases ran.
