# T-056F — Server settings UI

Status: done
Phase: 3
Size: M
Depends on: T-042, T-021, T-022, T-027
Parent: T-056
Paths: `apps/desktop/src/features/admin/`, `apps/desktop/src-tauri/src/server_settings.rs`, `crates/ss-server/src/http/server_settings.rs`

## Scope
Manage server name, description, attachment-backed icon and registration mode.
Display the connected server version and the certificate fingerprint approved on
this computer. Require MANAGE_SERVER, retain server-side authorization, and keep
operator configuration and certificate editing outside the UI.

## Acceptance
- Load existing settings before editing. Save fields atomically with an opaque
  revision; stale updates receive a conflict instead of overwriting newer edits.
- Preserve drafts on errors, explicitly confirm discarding unsaved changes, and
  discard editors when disconnected or access is revoked.
- Upload PNG/JPEG/WebP icons separately, bounded to 2 MiB and 4096×4096 pixels.
  Apply them only on Save, support removing an icon, and preserve uploaded IDs
  after failed saves so retries do not upload the same file again.
- Support invite-only and closed registration. Closed rejects new accounts even
  with an invite, while current sessions and existing-account login keep working.
  Reopening does not revoke previously created invites. Open registration remains
  unsupported by the existing authentication model and is not offered.
- Save settings persistently and audit updates. Unchanged startup configuration
  does not overwrite a name edited in the UI.
- Automated Rust/frontend tests, required checks, production build and rendered
  browser verification pass.

## Implementation — 2026-11-04
The sidebar Server settings button is visible to server managers. The native
bridge uses authenticated HTTPS pinned to the approved QUIC identity. Bearers
and remote icon URLs never reach the webview; previews use raster data URLs.

GET/PUT `/api/v1/server/settings` expose an allowlisted profile, never arbitrary
server-setting rows. PUT compares the revision inside the same transaction as the
field updates and `server.update` audit entry. GET uses a consistent read
transaction. New icons must be unattached uploads owned by the acting manager;
an already-selected icon can be retained by another manager. Uploads use the
existing file store, image sniffing, byte quota and orphan cleanup. The browser
cannot apply SVG or arbitrary remote image URLs.

Registration is checked both before password hashing and again when the account
is inserted, preventing a pending registration from slipping through a closure.
The existing onboarding probe already treats closed registration as login-only.

Startup remembers the last configured name. An unchanged configuration preserves
UI edits; explicitly changing `server.name` (including its environment override)
applies the new operator value at the next startup and advances the profile
revision. Existing connected Welcome metadata is refreshed on reconnect; public
server discovery reads the updated name immediately. Custom local server labels
are not changed by editing the remote server name.

T-056 remains open for the broader member administration/moderation UI. Installed
Windows/macOS/Linux acceptance remains part of the testing-app milestone.

## Validation
`just check` passed across the workspace. Frontend checks passed with 107 tests. Desktop checks passed with 78 native tests
and strict Clippy. Real QUIC/HTTPS tests verify authorized settings and icon
operations, stale-revision rejection, audit entries, TLS pin enforcement, and
closing/reopening registration while retaining existing-account access. A startup
regression test verifies that UI name changes survive unchanged configuration.
The production build passed with the existing bundle-size warning.

The rendered flow was verified in Chromium at `http://127.0.0.1:1420/settings-check`
at 1200×900 and 760×600. Page identity/content, console health, framework-overlay
absence and screenshot inspection passed, along with saves, permission denials,
conflicts, discard/reload, icon validation/upload/removal, Escape, focus restoration
and disconnect behavior. Browser plugin not available; verification used the
existing local Playwright installation with a temporary mock bridge, with the
real network paths verified separately above. Temporary preview scripts and
screenshots were kept outside the repository. Installed cross-platform acceptance
remains manual. No GitHub Actions, pushes or release workflows were triggered.
