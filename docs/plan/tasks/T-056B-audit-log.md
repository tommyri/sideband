# T-056B — Filterable audit-log viewer

Status: done
Phase: 3
Size: S
Depends on: T-042, T-030
Parent: T-056
Paths: `apps/desktop/src/features/admin/`, `apps/desktop/src-tauri/src/audit.rs`, `crates/ss-client-core/src/audit.rs`, `crates/ss-server/src/http/audit.rs`, `crates/ss-server/src/db/audit_log.rs`

## Scope
Add a permission-gated audit-log panel to the channel sidebar. Display timestamps,
actors (including system and departed users), actions, targets and expandable
plain-text event details. Support exact actor/action filters, local date/time
bounds, reset/refresh, and loading older pages.

Extend the authenticated REST endpoint with server-side filters, combined before
pagination. Use the existing pinned HTTPS transport and keep credentials native.
Clear displayed records after denial; hide them on disconnect or permission loss.
Discard stale requests after query changes or panel closure.

## Acceptance
- Server tests cover combined filters before limits, timestamp ties, cursor
  boundaries, exact action matching, invalid queries, unauthenticated denial,
  permission grant/revocation and certificate-pin rejection.
- Frontend tests cover filters, pagination/reset, safe details, stale responses,
  denial/recovery and disconnected/unauthorized controls.
- Browser checks cover desktop/minimum layout, actual interactions, keyboard
  dismissal, focus restoration and absence of runtime errors.
- `just check`, `just check-desktop` and the frontend build pass.

## Implementation — 2026-11-04
The native bridge fetches up to 50 rows per page with a ten-second timeout and a
1 MiB response cap. Dates are inclusive `since` and exclusive `until` in Unix
milliseconds. Actor and cursor inputs use ULIDs; response IDs remain base64 at the
REST boundary and are projected to ULIDs for the UI. Details render as escaped
text, never HTML. Refresh updates server activity; permission/resync events also
refresh the current query. Filters do not modify any server data.

T-056 remains open for server settings, roles, member moderation, channel editors,
webhooks and the permission-checkbox model. No platform-specific APIs are added;
installed-app cross-platform acceptance remains part of the testing-app milestone.

## Validation
`just check` and `just check-desktop` passed (79 frontend tests and 60 native
bridge tests). The production frontend build passed with the existing chunk-size
warning. Chromium verification at 1200×900 and 760×600 passed filters, pagination,
plain-text details, empty/error recovery, focus restoration, Escape and disconnect
controls, with no page or console errors. The browser used a mocked native bridge;
real QUIC/HTTPS integration separately verified filtering, permission grant/loss,
invalid queries, authentication and certificate pinning. No CI or release runs
were triggered.
