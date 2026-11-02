# T-043 — Onboarding UI: add server, trust fingerprint, register/login, multi-server

Status: in-progress
Phase: 3
Size: M
Depends on: T-042
Paths: `apps/desktop/src/features/onboarding/`

## Goal
A friend receives `host:port` + an invite code and is talking within two
minutes without reading docs.

## Scope
- Testing-app priority: deliver add-server/trust/register/login/reconnect
  first; this UI is no longer gated by T-018. Complete the remaining rail
  and checklist scope without blocking integration of the essential flow.
- "Add server" dialog: address input (accepts `host`, `host:port`,
  `ss://host:port`), live reachability check via `GET /api/v1/server`
  (name, registration mode, fingerprint).
- Certificate trust screen: show fingerprint (grouped hex), server name,
  "Trust this server" → pin; changed-fingerprint warning screen with explicit
  re-trust (red, explains the risk).
- Register (invite code, username, password with strength hint) and Login
  forms; errors mapped from `Error.code` to friendly text; remember session.
- Server rail: list of added servers with unread badges, reorder, remove
  (with confirm), per-server connection status indicator; reconnecting
  banner.
- First-run checklist after joining: pick mic/output, choose PTT key or
  voice activation (links to T-051 settings).

## Out of scope
Settings internals.

## Acceptance criteria
- Manual script in the task notes executed on all three OSes.
- Vitest component tests for form validation and error mapping.
- Keyboard-only flow works end to end.

## Notes from implementation
- Implemented address parsing (`host`, `host:port`, `ss://`, bracketed IPv6),
  debounced reachability checks, optional HTTPS port, explicit trust and red
  changed-certificate confirmation. Discovery sends no credentials: QUIC
  observes the certificate, then public metadata is fetched over HTTPS pinned
  to that exact certificate. It never persists trust implicitly.
- Added invite registration, password login, friendly protocol-code errors,
  persistent session reconnect and server rail ordering/removal. Removing a
  server disconnects it and deletes the saved token; the certificate pin stays
  to preserve certificate-change protection if the server is added again.
- Server list, session tokens and pins live in the native app-data directory,
  with atomic replacement and owner-only Unix file permissions. Tokens never
  cross the webview bridge; passwords are not stored. This is a local private
  JSON store, not an encrypted OS keychain; Windows inherits the user's app-data
  directory ACL. Reconnect after restart is explicit from the selected server.
- Rail badges count channels with unread messages, derived from server read
  cursors. Connection events show automatic retry state. Installed builds show
  an honest onboarding/checklist landing page instead of the scaffold's fake
  channels and members. Browser-only design preview remains available.
- Checklist entry points explain that device and talk-mode settings remain
  pending T-051/T-058; wire these to the real settings when those screens land.
- Automated coverage: component validation, explicit trust/re-trust, invite
  registration, friendly login errors, stale discovery responses; native
  persistence/reorder/removal and real local QUIC/HTTPS discovery → trust →
  registration → saved-session authentication → changed-pin detection.
- macOS packaged app: discovered the local test server, verified fingerprint,
  registered with an invite and reached the connected checklist. Fixed initial
  field focus and native dialog centering discovered during UI inspection.
- Verified app restart preserves the server and allows password-free saved
  session login; controlled server stop/start shows retry status and automatic
  recovery. Confirmed removal returns to the empty-server landing page.
- Full workspace `just check`, frontend type/lint/format/component checks and
  dependency graph checks passed. Tests requiring hardware remain explicitly
  ignored, not counted as acceptance evidence.
- Final frontend suite: 10 tests, including keyboard traversal of collapsed
  advanced options and focus restoration. Native macOS Tab/Enter login and
  Escape dismissal passed; the final dialog is centered and initially focuses
  the address field. Packaged debug app ZIP and checksums were generated.
- Remaining acceptance: run the script below on Windows and Linux, finish
  checklist integration with real settings, and record all platform results.
  Implementation is available for T-044 integration; this status does not block
  the next testing-app increment.

## Manual acceptance script
1. Install a labelled test build. Add a server with `host:port` or `ss://host:port`.
   Check metadata and compare the grouped fingerprint with the owner; cancel
   once, then explicitly trust. No password field appears before trust.
2. Create an account with an invite. Try an invalid username/password first;
   confirm actionable errors, then successful connection and checklist.
3. Quit and reopen. The server remains disconnected until Reconnect / log in →
   Continue → Reconnect with saved session. Confirm no password is required.
   Try an expired session and an incorrect password: both must return to login.
4. Add a second server; switch between them, move earlier/later, restart and
   verify order. Confirm removal disconnects and forgets the saved session.
5. Restart the server with the same certificate: verify reconnecting banner and
   automatic recovery. Replace its certificate: verify both fingerprints and
   the red warning; cancel must preserve the old pin, explicit replacement
   permits login. Never re-trust an unexplained change on a real server.
6. Repeat with keyboard navigation (Tab / Shift-Tab, Enter, Escape). Confirm dialog
   focus stays inside and returns to the invoking control. Check layout at
   minimum window size. Exercise both checklist entry points; they must not
   claim device setup is complete until settings are implemented.
