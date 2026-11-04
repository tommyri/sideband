# T-045A — Live text chat foundation

Status: done
Phase: 3
Size: M
Depends on: T-042, T-026
Parent: T-045
Paths: `apps/desktop/src/features/chat/`, `apps/desktop/src-tauri/src/chat.rs`, `crates/ss-client-core/src/state/`

## Goal
Make installed clients usable for real text conversations while cross-OS voice
testing continues separately. This is the first increment of T-045.

## Scope
- Text-channel UI backed by the native client's authoritative message cache.
  Explicitly displayed live messages enter retained history so the 200-message
  rolling window does not remove already displayed rows; edits/deletes still
  reconcile in the core cache.
- Virtualized, dynamically measured list; stable history prepends and follow only
  when already at the bottom; explicit older-page loading and jump to latest.
- Immutable account name alongside display name, author grouping within seven
  minutes, day boundaries, and an initial unread divider.
- Safe plain-text messages; no raw HTML or rich rendering in this increment.
- Send, reply, edit own recent messages, confirmed delete, and copy text.
- Multiline composer; Enter sends, Shift+Enter inserts a newline, IME composition
  does not send. Preserve failed drafts and reuse a nonce on unchanged retries.
- Read cursors advance only at the bottom in a focused, visible window; existing
  channel/server unread indicators refresh from native events.
- Permission/disconnection guards, errors and retries, and isolation from stale
  responses after switching channels.

## Acceptance
- Automated composer/cache tests cover retries, identity grouping, stale replies,
  server/channel isolation, IME, editing, and empty-history termination.
- Native integration test exercises send/edit/read/delete against a real server.
- Rendered browser check covers safe text, send/edit/reply/delete, history scroll
  anchoring, read suppression away from bottom, and bounded DOM with 10,000 rows.
- `just check` and `just check-desktop` pass.

## Limits and follow-ups
T-045B owns rich text/mentions, typing, additional composer shortcuts, complete
keyboard navigation, unloaded reply-target lookup, copy-link/id, and deleted-own
message placeholders. T-045C owns attachment/link-card UI. Real cross-platform
WebView behavior and the parent's 60fps target still require acceptance testing;
a bounded DOM browser check does not certify hardware frame rates. Channel drafts
are retained when entering/canceling edits, but not across channel navigation.

## Verification — 2026-11-04
- `just check`: passed (workspace fmt, clippy, and tests; existing opt-in/manual
  tests remain ignored).
- `just check-desktop`: passed; 49 frontend tests and 43 native desktop tests.
- Real local-server integration verifies the new bridge's send/edit/read/delete
  projection. A core regression test covers displayed-message retention after
  live-window eviction, followed by editing and deletion.
- Headless Chromium with a simulated native bridge: send/edit/reply/delete,
  literal HTML rendering, stable history prepend, no read advancement while
  scrolled away, read advancement after returning to latest, and fewer than 35
  rendered rows for 10,000 cached messages. Inspected desktop/narrow screenshots.
- Integrated channel workspace keeps the list and composer visible at 1440×900
  and the minimum 760×600 window. Browser plugin was unavailable; used local
  Playwright. These checks do not replace installed WebView acceptance.
- No GitHub Actions run or release dispatched for this task.
