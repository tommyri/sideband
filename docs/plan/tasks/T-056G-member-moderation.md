# T-056G — Member administration and moderation

Status: done
Phase: 3
Size: M
Depends on: T-042, T-030, T-056E
Paths: `apps/desktop/src/features/admin/`, `apps/desktop/src-tauri/src/moderation_admin.rs`, `crates/ss-client-core/src/moderation_admin.rs`, `crates/ss-server/src/http/moderation_admin.rs`

## Goal
Finish the T-056 member administration surface without duplicating role assignment.

## Delivered
- Permission-gated Members & moderation dialog with literal username/display-name
  search, role chips, account creation date, last session activity, and bounded
  50-row cursor pagination. Role assignment stays in Roles and permissions.
- Kick and ban confirmations require the target username, accept an optional
  500-character reason, and explain reconnect behavior. Ban duration presets are
  1 hour, 1 day, 7 days, 30 days and permanent. Native code computes expiration
  at submission and validates the preset; existing server handlers remain
  authoritative and audited.
- Active bans show reason, creator, creation time, expiry and hierarchy-gated
  unban. Expired/lifted bans are excluded. Unban uses the member identifier and
  lifts all active bans for that member, matching the existing protocol.
- Server-projected per-member capabilities protect self and higher/equal roles;
  disabled/banned accounts cannot be kicked or banned from this list. The server
  checks authority again at mutation time. Denials retain the draft; disconnects,
  permission loss, unmounts and server switches invalidate the displayed controls
  or ignore stale results. Refresh fetches changes from other moderators.
- Pinned authenticated HTTPS read APIs expose only administrative metadata;
  account passwords, session credentials, devices and tokens never reach the UI.
  The native bridge releases the clients lock before awaiting HTTP responses.

## Timestamp semantics
The existing database does not persist precise last-online/offline history.
The UI intentionally labels the available maximum session activity timestamp
**Last session activity**, explains its meaning, and shows Unavailable when no
session remains. It does not invent an exact last-seen time. Joined means account
creation on this server. Accurate durable presence history is a separate feature.

## Validation
Frontend type checking, formatting, lint and 112 tests pass. Native preset/DTO
checks and the generated ServerView binding pass. Real QUIC/HTTPS tests cover
scoped access, kick-only permissions, revocation, hierarchy rejection, literal
search, metadata, timed bans/unban, page bounds and expired bans. The HTTP
route authorization audit passes. Strict Clippy passes for the affected crates.

Chromium verified the rendered dialog at 1200×900 and 760×600, including timed
ban denial/retry with the draft preserved, unban, Escape and focus restoration;
console and framework-overlay checks passed and screenshots were inspected.
Browser plugin was unavailable, so an existing local Playwright installation and
a temporary mock bridge were used; real network behavior was tested separately.
Preview artifacts stay outside the repository. Cross-platform installed-app
acceptance remains in the testing-app milestone. No Actions or releases ran.
