# T-056C — Webhook management UI and metadata history

Status: done
Phase: 3
Size: S
Depends on: T-042, T-031
Parent: T-056
Paths: `apps/desktop/src/features/admin/`, `apps/desktop/src-tauri/src/webhooks.rs`, `crates/ss-client-core/src/webhooks.rs`, `crates/ss-server/src/http/webhook_admin.rs`

## Scope
Create named incoming webhooks for visible text channels, reveal/copy each posting
URL once, list metadata from previous sessions, and confirm revocation. Show the
creator, destination, creation/revocation timestamps and status. Gate the UI and
server metadata endpoint with MANAGE_SERVER; display server denials.

Add authenticated bounded metadata pagination. Tokens, hashes and executable URLs
never appear in history responses. Build the one-time URL from the connected
HTTPS origin, including its configured port. Keep credentials out of persistent
UI state and discard them on closure, disconnect, account switch or permission loss.

## Acceptance
- Real client/server tests verify history and pagination, no credentials in JSON,
  authenticated/authorized access, grant/loss of permissions, correct URL origin,
  create → post → revoke → rejected post, and wrong-certificate rejection.
- Frontend tests verify validation, channel choice, copying/manual fallback,
  reveal-once lifecycle, revoke confirmation/denial/retry, pagination, access loss
  and ignoring late creation responses after disconnect.
- Browser checks verify creation/copy/closure/revoke, keyboard dismissal and focus
  restoration at desktop and 760×600 minimum size, without runtime errors.
- `just check`, `just check-desktop` and the production frontend build pass.

## Implementation — 2026-11-04
The Webhooks button opens a manager for accounts with MANAGE_SERVER. The default
channel is the first visible text channel; voice channels and DMs are excluded.
Names are checked locally, with the server remaining authoritative for hidden
characters and destination validation. Creation and revocation use existing QUIC
commands; mutations are never automatically retried. Interrupted creation advises
refreshing history and revoking a webhook whose one-time URL was lost.

GET `/api/v1/webhooks` lists newest-first by ULID, default 50 and maximum 100 rows.
The desktop fetches 50 at a time over pinned HTTPS with a ten-second timeout and
256 KiB response cap. Deleted/unavailable channels retain their ID in history.
Revocation prevents further posts without removing existing messages. Refresh
updates remote changes; permission/resync events refresh metadata too.

T-056 remains open for server settings, roles, member moderation, channel editors
and the permission-checkbox model. Installed-app cross-platform acceptance is
still tracked by the testing-app milestone.

## Validation
`just check` and `just check-desktop` passed, including the route authorization
audit, real QUIC/HTTPS integration, 85 frontend tests and 62 native desktop tests.
The production frontend build passed with the existing bundle-size warning.
Chromium at 1200×900 and 760×600 passed page/content/console checks, text-channel
selection, name validation, create/copy, URL removal on closure/disconnection,
revoke confirmation/denial/retry, focus restoration and Escape. Browser validation
used mocked native and clipboard adapters; real server integration separately
verified credential handling, posting and revocation. Native clipboard behavior
and installed Windows/macOS/Linux acceptance remain part of manual app testing.
The Browser plugin was unavailable, so the existing local Playwright installation
was used. No GitHub Actions or release runs were triggered.
