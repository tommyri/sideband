# T-096 — Chat history robustness, user-visible notices, export redaction

Status: todo
Phase: 3
Size: M
Depends on: T-045A, T-076, T-095
Paths: `crates/ss-client-core/src/state/`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src-tauri/src/chat.rs`, `apps/desktop/src/features/chat/`, `apps/desktop/src/lib/events.ts`, `apps/desktop/src/features/onboarding/ServerHome.tsx`, `apps/desktop/src/features/testing/report.ts`

## Goal
Opening a channel always shows its messages, the app tells the user why
something happened, and an evidence export contains no addresses.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
exercised at runtime. Shares command modules with T-095; run after.

## Scope
- `StateStore::history` rejects a page when `revision` changed, and every
  applied event bumps the revision. Any typing, presence or voice event
  during the fetch fails "Load older", jump-to-message and the first open
  with "state changed while loading history; retry the page". On first open
  the view stays undefined, the list is empty and the composer disabled
  until Retry or the next chat event; the error banner is not cleared by a
  later success. Scope the check to the channel or retry in the shell with
  a bound, and always refresh the view after a failed page.
- Jump-to-message renders the 50-message "around" window directly above
  the latest messages with no marker, and "Load older" pages only before
  the oldest row, so the messages in between cannot be reached. Track
  loaded ranges and render a gap row with load-newer/load-older, or replace
  the view with the around-window.
- `Notice`, `Removed { banned, reason }`, `ServerError` and
  `Failed { message }` are forwarded by the shell and not handled in
  `lib/events.ts`; `ServerHome.tsx` shows fixed text and ignores
  `status.message`. A kicked or banned user sees a generic "Connection
  ended" and "Could not rejoin voice" is dropped. Surface them per server.
- A lagged event stream (`Lagged` in the forwarder) skips events with no
  signal, leaving stale UI. Emit a resync event and reload state.
- `TestingPanel.tsx` tests `e instanceof Error`, which is never true for a
  Tauri `ApiError`, so every export failure reads "Check that Downloads is
  writable", including the 16 MiB limit and consent errors.
  `DiagnosticsPanel.tsx` has a bare `catch` with the same text. Use
  `errorText(e)`.
- Evidence redaction (`report.ts`) matches only `https?` and the old
  `socialspeak` scheme and IPv4. The app's own `sideband://` and `ss://`
  links, bare `host:port`, hostnames and IPv6 literals pasted into notes
  are exported verbatim. Redact any `scheme://` token, `host:port` and IPv6
  literals. `05-testing-app.md` requires this.
- Unconfirmed: a failed `refreshVoice` nulls `active` and hides the voice
  dock (mute, leave, stop sharing) while the native session continues.
  Keep the dock while a session exists.

## Out of scope
New chat features. Server-side history API changes other than what the gap
handling needs.

## Acceptance criteria
- Test: history load succeeds while unrelated events arrive continuously.
- Test: after jumping to an old message, every message between it and the
  newest can be loaded.
- Component tests: ban reason and rejoin failure are shown.
- `report.test.ts` seeds `sideband://`, `ss://`, `host:port`, a hostname
  and an IPv6 literal and finds none in the export.

## Notes from implementation
