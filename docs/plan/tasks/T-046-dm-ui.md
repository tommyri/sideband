# T-046 — DM UI

Status: done
Phase: 3
Size: S
Depends on: T-045A, T-028
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

- The chat dependency is now T-045A, the completed reusable live-chat foundation.
  T-045B/C rich rendering and attachments will benefit channels and DMs together;
  they are not prerequisites for the DM list and membership controls.
- Per-server direct-message list sorted by activity, presence indicators,
  unread/mention badges, member-menu Message action, and searchable participant
  picker. Group creation supports 2–9 other people and an optional name.
- Shared chat component handles history, messages, replies, editing/deletion,
  and read cursors. Group headers show participants, creator-only additions
  (10 people maximum), renaming, and confirmed leave. Membership and command
  authorization remain server-enforced.
- Group creation and naming are separate protocol operations. A failed name
  update returns the created conversation plus a warning and Rename retry;
  it is never presented as a failed creation that should be repeated.
- Live mention-count deltas now update the shared core read-state mirror for
  channels and DMs, with cached-echo deduplication and edit/delete handling.
  Authoritative read-state events still replace local counts.
- Compact chat layout preserves usable message/composer space at 760×600;
  the server member panel is hidden while chatting at narrow widths. The
  new-conversation picker remains available there.
- Native notification toasts/sounds are still T-055 work for both channels and
  DMs; this task shares the existing unread/read-state behavior. Installed
  cross-platform acceptance remains part of the testing milestone.

## Verification — 2026-11-04
- `just check` passed: workspace formatting, clippy, and automated tests.
  Existing opt-in/manual tests remain ignored.
- `just check-desktop` passed: 55 frontend tests and 48 native desktop tests;
  frontend checks rerun after the final leave-confirmation copy update.
- Native integration uses four accounts against a real local server: repeated
  partner creation returns the same channel; group naming, creator-only adds,
  member renaming/leaving, partial naming failure/recovery, incoming mention
  counts, and read-cursor clearing all pass.
- Core regression covers live mention deduplication, edits/deletions,
  already-read and own messages, and the server's @everyone boundary rules.
- Headless Chromium with a simulated native bridge verifies group creation,
  sending, adding members, renaming, confirmed leave, and 1:1 reuse. Desktop
  (1440×900) and minimum-window (760×600) screenshots inspected; sending also
  verified at minimum size with participants expanded. No runtime errors.
  Browser plugin unavailable; used local Playwright.
- No GitHub Actions or release workflow dispatched. Native cross-platform
  WebView acceptance remains in the broader testing-app milestone.
