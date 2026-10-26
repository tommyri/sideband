# T-046 — DM UI

Status: todo
Phase: 3
Size: S
Depends on: T-045, T-028
Paths: `apps/desktop/src/features/dm/`

## Goal
Private conversations feel like channels but live in a "Direct Messages"
area with their own list.

## Scope
- DM list section at the top of the server rail or a dedicated "Home" view
  per server (DMs are per server since there's no central identity): sorted by
  last activity, unread badges, online dot.
- Start a DM from member hover card / context menu; group DM creation dialog
  (pick members, name).
- Reuse the T-045 chat view with DM-specific header (participants, add
  member, leave group).

## Out of scope
Cross-server identity.

## Acceptance criteria
- Creating a DM with an existing partner opens the existing channel.
- Unread and notification behaviour identical to text channels.

## Notes from implementation
