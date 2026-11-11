# T-096 — Chat history robustness, user-visible notices, export redaction

Status: in-progress
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

Implemented 2026-11-11. Each finding got a test that failed first, for the
stated reason, before the fix. None was refuted; two are slightly different
from the review.

| Finding | Test | Before the fix |
|---|---|---|
| Any event fails a history page | `state::tests::history_loads_while_unrelated_events_arrive_continuously` (a thread applies typing, presence, voice and other-channel messages while three pages load) | `StateChanged` after one typing event in another channel |
| First open after a failed page | `chat.test.tsx` "shows the cached messages when the first page fails" and "clears a snapshot error once a later snapshot succeeds" | view `undefined`; error kept after a good snapshot |
| Jump gap | `state::tests::after_a_jump_every_message_up_to_the_newest_can_be_loaded`, `loading_older_from_the_newest_run_joins_a_jumped_to_run` | paging older from the top row stopped with 100 of 300 cached |
| Notices not shown | `ServerHome.test.tsx` (ban reason, rejoin failure, server error with wait, failed reason) | nothing shown |
| `Lagged` skipped silently | `commands::tests::a_lagged_forwarder_resyncs_the_webview` | first item was the next event, no resync |
| Export errors | `TestingPanel.test.tsx`, `DiagnosticsPanel.test.tsx` (an `ApiError` rejection) | "Check that Downloads is writable" |
| Redaction | `report.test.ts` seeds `sideband://`, `ss://`, `socialspeak://`, `quic://`, `host:port`, `gaming-pc.local`, a hostname, bracketed and bare IPv6 with zone and port, IPv4 with port | `sideband://`, `ss://`, IPv6, host names and `host:port` exported; `quic://203.0.113.9:5781/path` became `quic://[redacted address]/path` |
| Unconfirmed: voice dock | `voice/store.test.ts` | dock hidden on any refresh error |

Differences from the review:
- A live kick or ban ends in `Disconnected`, not `Failed`, so the home page
  showed no reason at all rather than "Connection ended". "Connection ended"
  is for terminal resume refusals; it now includes `status.message`.
- The voice dock item is real in code but narrow. The shell always stops
  voice before it drops a server's client, so `get_server_view` fails while
  a session exists only on a malformed id in server data, a storage error or
  an IPC failure. The dock now stays unless the error is `disconnected`.

**History revision.** Scoped in `StateStore`, no shell retry. Each
`MessageCache` records the store revision of the last live event that
changed it (`changed`). A fetch takes a `Ticket` (generation and revision);
`history` refuses the page only if a Welcome was installed since or a live
message event changed that channel since. Typing, presence, voice, roles and
other channels no longer matter. Pages do not invalidate each other, and the
T-089 resync merge does not mark the channel: both are reads of the
database in the same session, and live changes during either are marked
separately. `useChat` refreshes the projection after every page, failed or
not, and a snapshot error is cleared by the next good snapshot; a page
error stays until the next attempt.

**Jump to message: the choice is to replace the view**, not a gap row in one
list. `MessageCache` now tracks inclusive id ranges known to be complete
(`known`). A `before` page covers `[first, before)`, an `after` page
`(after, last]`, an around page `[first, last]`; a short page reaches the
channel's end in its direction, and the newest page and live creates reach
the live edge. Overlapping or touching ranges merge. `segment(anchor)`
returns the gapless run holding `anchor`, or the newest run, with `start`
and `latest`. `chat_snapshot` takes `around` and projects one run, so a jump
shows only the run around the target; a "Load newer messages" row below it
calls the new `load_newer` command (`Client::load_newer`, the server's
existing `after=`) until the run reaches the newest, and "Jump to latest"
goes back. Paging older from the newest run joins the jumped-to run when it
reaches it. Reading the run around a target never marks it read, and the
list does not follow appends there, so newer pages load below the reader.
"Beginning of conversation" now comes from `start`, not a page length.

Also found: a channel not on screen that got more than 200 messages lost the
oldest live ones (not retained in `history`), leaving a silent hole between
earlier loaded pages and the live window. Eviction now splits the range
(`live_messages_evicted_unseen_leave_a_loadable_gap`). With the resync
(T-089): pages cover their span; when the page budget runs out, coverage of
the dropped older messages is dropped too, and a resync page never claims
the channel's start from its length, since live deletes can shorten it
(`a_resync_that_drops_older_messages_forgets_their_coverage`).

**Notices.** `lib/notices.ts` keeps up to five notices per server from
`removed` (reason, banned or removed), `notice` (server notices, "Could not
rejoin voice: …", "Voice continued on another device", "Message resync
deferred: …") and `serverError` (with "Try again in N s" from the new
`retryAfterMs`). `ServerNotices` renders them, dismissible, on the server
home and in the channel workspace. A removal clears when the server is
connected again; other notices stay, since the rejoin failure is sent just
before `Connected`.

**Lagged.** The forwarder's `next_event` turns `Lagged` into `Resynced`,
which the webview already handles by re-reading chat, voice and the server
list; `notifications::observe` sees it too, so the badge is refreshed and
a DnD presence is re-sent, as after a reconnect resync.

**Redaction.** Any `scheme://` token, IPv6 literals (with `::` or eight
groups, brackets, zone and port), IPv4 with port, `host:port` where the
host has a letter, and dotted host names. Times such as `12:30:45`, ratios
and Rust paths such as `a::b` survive. Dotted words such as `report.json`
are redacted too; that is the cost of catching bare host names.
`TestingPanel` and `DiagnosticsPanel` show `errorText(e)` (now in
`lib/errors.ts`, re-exported by `chat/api.ts`).

**Files outside the Paths**
- `crates/ss-client-core/src/history.rs`: tickets, `Fetch`, `load_newer`.
- `apps/desktop/src-tauri/src/types.rs`: `retry_after_ms` on `ServerError`;
  `lib.rs`: one line registering `load_newer`. `HistoryRequest` is
  unchanged, so `voice.rs` (T-103) is untouched.
- `apps/desktop/src/lib/generated/{ChatSnapshot,ClientEventPayload}.ts`
  (generated).
- New: `src/lib/errors.ts`, `src/lib/notices.ts`,
  `src/features/onboarding/ServerNotices.tsx`, `ServerHome.test.tsx`,
  `src/features/voice/store.test.ts`.
- `src/features/channels/ChannelWorkspace.tsx` (two lines),
  `src/features/testing/TestingPanel.tsx` and its test, `report.test.ts`,
  `src/features/diagnostics/DiagnosticsPanel.tsx` and its test,
  `src/features/voice/store.ts`.
- `attachments.rs` and the `can_*` derivation in `chat.rs` are unchanged
  (T-104, T-105); `ChatSnapshot` gained `start` and `latest` and an allow
  for `struct_excessive_bools`.

**Open questions and follow-ups**
- A live event in the same channel during a fetch still refuses the page,
  so "Load older" in a very busy channel can need a retry. Merging the page
  while skipping ids touched since the ticket (as the resync's `live` map
  does) would remove that.
- Coverage is not cut at a reconnect: until the resync's first page lands,
  and if the resync is deferred, the newest run is assumed gapless, as
  before this task.
- `SubscriberHint` declares `#[ts(rename = "maxHeight")]` and friends, but
  serde sends `max_height`; the TypeScript type is wrong. Nothing reads it.

**Manual checks**
- In a channel with a few hundred messages, follow a reply link to an old
  message: only its run shows; "Load newer messages" pages down without
  jumping past the loaded rows; the run joins the newest and the row goes.
  "Jump to latest" returns. Nothing is marked read while viewing old runs.
- Kick and ban a test account: the reason shows on the server home. Rejoin
  voice after a restart with the channel full or the permission removed:
  the notice shows in the workspace.
- Export a report with an `ss://` link and a host name in the notes.
