# T-108 — Desktop follow-ups from the review waves

Status: todo
Phase: 3
Size: S
Depends on: T-096, T-105
Paths: `apps/desktop/src-tauri/src/`, `apps/desktop/src/features/`

## Goal
The desktop gaps that earlier tasks noted for someone else, and nobody
picked up, are closed or explicitly dropped.

Source: T-099, from the notes of T-049, T-086, T-091 and T-101, checked
against the code at `0d0cb16`. Starts after T-096 and T-105, which edit the
same shell modules; drop any item they already covered.

## Scope
- T-049 says the app rechecks Input Monitoring "on focus" and that T-058
  would wire it. Nothing does: `lib.rs` `window_event` handles only
  `CloseRequested`, and a grant is noticed only through "refresh
  capabilities" or a restart. Recheck capabilities when the main window
  gains focus, or change T-049's note.
- The desktop never calls `list_dms` (`ListDms`, T-091), so DMs beyond the
  100 in `Welcome` (`Snapshot.direct_messages_next`) cannot be opened. Load
  further pages on scroll in the DM list.
- `ClientEvent::ServerError`'s `retry_after_ms` is dropped on its way to the
  UI (T-101). Show the wait, unless T-096's notices already do.
- `role_admin::mask` rejects permission bits it has no name for (T-101);
  `02-protocol.md` §7 says clients keep unknown bits. Keep them through an
  edit, unless T-105 already does.
- After a `ChannelUpdated` that changes the voice profile, the desktop does
  not rejoin; T-086 made receivers follow the sender's frame size instead.
  Confirm nothing else depends on a rejoin, then drop this item or fix it.

## Out of scope
Notification policy (T-102) and voice following (T-103).

## Acceptance criteria
- A shell test covers the focus recheck; a frontend test loads a second DM
  page.
- Each remaining item has a test or a recorded reason it was dropped.

## Notes from implementation
