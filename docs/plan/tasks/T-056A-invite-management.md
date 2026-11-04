# T-056A — Invite management UI and scoped history

Status: done
Phase: 3
Size: S
Depends on: T-042, T-030
Parent: T-056
Paths: `apps/desktop/src/features/admin/`, `apps/desktop/src-tauri/src/admin.rs`, `crates/ss-client-core/src/invites.rs`, `crates/ss-server/src/http/invites.rs`

## Scope
Create invites with use/expiry limits; reveal/copy a newly created code once;
list creator, use count, creation/expiry and active/expired/exhausted/revoked status;
confirm revocation. Gate the entry point and actions by server permissions and
connection state, and show server denials without losing the form or crashing.

Add bounded, authenticated, paginated metadata retrieval so invites from previous
sessions can be managed. Server managers see all records; invite creators see only
their own. Follow the server's revoke rule exactly: your own invite requires
CREATE_INVITES, another creator's invite requires MANAGE_SERVER. Neither codes nor
hashes appear in list responses. Only the create response reveals the code.

## Acceptance
- Real client/server tests cover scope, pagination, unauthenticated/unauthorized
  denial, permission loss, revocation, query bounds and wrong-certificate rejection.
- Frontend tests cover limits/status/permissions, one-time codes, copying failures,
  revoke confirmation/denial, pagination and clearing rows after access loss.
- Browser checks cover create/copy/revoke/error recovery, focus/keyboard dismissal,
  disconnection and the minimum 760×600 window size.
- `just check` and `just check-desktop` pass.

## Implementation — 2026-11-04
The Invites button is available in the channel sidebar to authorized accounts.
The modal defaults to a single use and a one-day expiry, supports unlimited uses
and no expiry, and keeps the newly created code only until dismissal/closure.
Refresh fetches current usage; older pages load on demand. Live invite and role
changes trigger refresh, and disconnected controls are disabled.

`GET /api/v1/invites` returns at most 100 records (desktop requests 50) over the
same pinned HTTPS transport used for attachments. The native bridge projects
metadata separately from the one-time create response. Desktop list responses
are capped at 256 KiB and ten seconds. The shared transport continues to reject
redirects and environment proxies and keeps bearer credentials native.

T-056 remains open for the rest of the admin UI. This increment introduces no new
platform-specific APIs; clipboard access has a manual-copy fallback.

## Validation
`just check` and `just check-desktop` passed, including 74 frontend tests and 58
native desktop tests. The production frontend build passed with the existing
bundle-size warning. Browser verification used a mocked native bridge at
1200×900 and 760×600 and passed page/console checks, limits, create/copy, removal
of codes on close, focus restoration, revoke confirmation/denial/retry, disconnect
controls and Escape. The authenticated API was separately verified against a real
local server. The endpoint is registered in the mandatory authorization audit.

Full-suite testing also exposed the pre-existing control close-reason race
recorded in T-060. It was fixed with a deterministic regression in a separate
commit; both that regression and the unchanged voice-resume test passed in the
final complete workspace run.
