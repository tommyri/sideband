# T-056D — Channel management UI

Status: done
Phase: 3
Size: M
Depends on: T-042, T-023
Parent: T-056
Paths: `apps/desktop/src/features/admin/`, `apps/desktop/src-tauri/src/channel_admin.rs`

## Scope
Create categories, text and voice channels; edit names/topics and voice settings;
move channels between categories and the root; reorder siblings; confirm deletion.
Expose voice profile, a bitrate slider within the profile range, default-bitrate
reset and optional user limits. Require MANAGE_CHANNELS in the entry point and
native bridge while retaining server-side authorization for every mutation.

Per-channel role overrides belong to the roles/permissions increment. Existing
channel types are fixed in this UI; choose the type at creation. Moving is a
separate operation from editing settings, so no multi-command partial save occurs.

## Acceptance
- Creation supports all three guild kinds, excludes DMs and nested categories.
- Edit loads current settings, supports clearing topics and restoring voice
  defaults, preserves the form on denial and detects observed concurrent edits.
- Moving to root and sibling ordering work through real server events, including
  updates seen by a second client. Only visible channels are listed; hidden
  siblings still count in server positions.
- Category deletion explicitly warns about all descendants, including hidden
  channels, and requires typing the category name. Messages become inaccessible
  and affected voice members are removed by the existing server behavior.
- Disconnected/unauthorized controls are disabled; server errors are actionable;
  requests are not retried automatically.
- `just check`, `just check-desktop`, production build and browser checks pass.

## Implementation — 2026-11-04
A Manage channels button opens the administration panel. The native bridge
projects the live guild tree and profile ranges from ss-core. No new server API
or database schema is needed. Existing personalized channel events synchronize
positions and visibility; the panel refreshes on channel, role and resync events.

New channels append to their destination. Up/down buttons move past visible
siblings; Move supports an explicit category/root and one-based position. The
server clamps positions beyond the end. A setting changed by another admin while
editing produces a conflict notice and requires reopening the editor. This is
client-side conflict detection, not an atomic compare-and-swap protocol.

T-056 remains open for server settings, roles/permissions, member administration
and channel permission overrides. Installed cross-platform acceptance remains
part of the testing-app milestone.

## Validation
`just check` and `just check-desktop` passed, including 92 frontend tests and 69
native desktop tests. A real two-client native-bridge test covers creation,
clearing settings, moving to root, sibling position synchronization, authorization,
invalid nesting and category cascade deletion. The production frontend build
passed with the existing bundle-size warning.

Chromium checks at 1200×900 and 760×600 passed creation and profile constraints,
edit denial/retry, move/root/order, observed concurrent edit handling, typed
category deletion, disconnect controls, focus restoration and Escape. Page identity,
content, framework-overlay and console checks passed. The browser used a mocked
native bridge, while the native integration test used a real local QUIC server.
The Browser plugin was unavailable; validation used the existing local Playwright
installation. Installed cross-platform and audible voice-setting acceptance remain
manual testing work. No GitHub Actions or release workflows were triggered.
