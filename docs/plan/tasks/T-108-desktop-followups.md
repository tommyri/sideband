# T-108 — Desktop follow-ups from the review waves

Status: awaiting-acceptance
Phase: 3
Size: S
Depends on: T-096, T-105
Paths: `apps/desktop/src-tauri/src/`, `apps/desktop/src/features/`

## Goal
The desktop gaps that earlier tasks noted for someone else, and nobody
picked up, are closed or explicitly dropped.

Source: T-099, from the notes of T-049, T-086, T-091 and T-101, checked
against the code at `0d0cb16`. Starts after T-096 and T-105, which edit the
same shell modules; drop any item they already covered.

## Scope
- T-049 says the app rechecks Input Monitoring "on focus" and that T-058
  would wire it. Nothing does: `lib.rs` `window_event` handles only
  `CloseRequested`, and a grant is noticed only through "refresh
  capabilities" or a restart. Recheck capabilities when the main window
  gains focus, or change T-049's note.
- The desktop never calls `list_dms` (`ListDms`, T-091), so DMs beyond the
  100 in `Welcome` (`Snapshot.direct_messages_next`) cannot be opened. Load
  further pages on scroll in the DM list.
- `ClientEvent::ServerError`'s `retry_after_ms` is dropped on its way to the
  UI (T-101). Show the wait, unless T-096's notices already do.
- `role_admin::mask` rejects permission bits it has no name for (T-101);
  `02-protocol.md` §7 says clients keep unknown bits. Keep them through an
  edit, unless T-105 already does.
- After a `ChannelUpdated` that changes the voice profile, the desktop does
  not rejoin; T-086 made receivers follow the sender's frame size instead.
  Confirm nothing else depends on a rejoin, then drop this item or fix it.

## Out of scope
Notification policy (T-102) and voice following (T-103).

## Acceptance criteria
- A shell test covers the focus recheck; a frontend test loads a second DM
  page.
- Each remaining item has a test or a recorded reason it was dropped.

## Notes from implementation

Implemented 2026-11-12. Each fixed item got a test first, which failed for
the stated reason before the fix.

| Item | Outcome | Test | Before the fix |
|---|---|---|---|
| Focus recheck (T-049) | Fixed | `input::tests::regaining_focus_restarts_a_listener_that_waited_for_access_once_it_is_granted`, `tests::focusing_the_main_window_rechecks_input_access` (`lib.rs`) | "access granted in System Settings starts the listener"; `rechecks_input("main", Focused(true))` false |
| `ListDms` paging (T-091) | Fixed | `dm.test.tsx` "loads the next page of conversations when the end of the list scrolls into view"; `voice_view` snapshot test checks `more_direct_messages` | `load_dms` never called |
| `retry_after_ms` (T-101) | Dropped: T-096 did it | `ServerHome.test.tsx` "shows unsolicited server errors with the wait the server asked for" | – |
| Unknown bits through `mask()` (T-101, T-105) | Fixed | `role_admin::tests::edits_keep_permission_bits_this_build_has_no_name_for`, `the_server_validates_the_bits_the_shell_passes_through` | "Invalid permissions" (`invalid_role`) from the shell |
| Rejoin on a profile change (T-086) | Dropped | – | – |
| Scroll after an over-budget resync (T-107) | Fixed | `chat.test.tsx` "moves the reader only when a resync dropped the row being read", `resync-scroll.test.tsx` | `afterTrim` missing; no scroll after the run was replaced |
| `voice_view` `can_manage_channels` (T-105) | Done: `Authority::can_manage_channels()` | existing `voice_view` and `channel_admin` tests | – |

**Focus recheck.** `window_event` calls `input::recheck` when the main
window gains focus. It starts the listener again only when it stopped for
want of access (`Runtime::retry_on_focus`): a start that found the
permission missing, or a running listener the backend stopped (revoked
access, secure input). It also needs the capability check, which never
prompts, to report the access present. A start while access is still
missing would prompt again, so nothing happens on that focus. A queue
overflow and a portal session (the compositor's, where a restart can
prompt) are not retried. Other start failures, such as a compositor that
owns shortcuts, are not retried on every focus either. "Retry saved
bindings" still restarts by hand.

**DM pages.** `ServerView.more_direct_messages` is
`State::direct_messages_next.is_some()`. The new `load_dms` command calls
`Client::list_dms` (one page of at most 100). `DmList` ends in a "Load older
conversations" button. An `IntersectionObserver` on it loads the next page
when it scrolls into view, and it is also a button for the keyboard. Only
one load runs at a time. The observer is rebuilt after each page, so it
fires again when the end is still in view. `loadOlderDms` rereads the server
view after the page. A failure shows in the list and the button retries it.

**Retry hint: dropped.** T-096 covered it: `types.rs`
`ClientEventPayload::ServerError.retry_after_ms`, `commands.rs` forwards it,
and `lib/notices.ts` appends "Try again in N s", with the test above.

**Unknown permission bits.** `role_admin::mask` now accepts any decimal
`u64` and rejects anything else. Role create and edit, and channel
overrides, keep bits this build has no name for. The frontend's `BigInt`
toggles already kept them (T-101). The server still validates every mask:
`handlers/roles.rs` `bits()` (`permissions_from_bits`) answers `INVALID`
"unknown permission bits" for a bit it does not know, in `CreateRole`,
`UpdateRole` and `SetChannelOverride`. `tests/permissions.rs` covers this
for `CreateRole` ("unknown permission bits are rejected"). The new
real-server test shows that an edit from the desktop now gets that server
answer (`server_4`) instead of the shell's own refusal. A newer server that
names the bit accepts it.

**Rejoin on a profile change: dropped.** Nothing depends on a rejoin. The
server fixes a seat's codec channels and bitrate cap from the profile at
join (`voice/session.rs`, `codec_channels` and `max_bitrate_bps`), and the
member's encoder keeps the profile it joined with, so the two still agree
after a switch. Receivers rebuild for the sender's frame size and channels
from the Opus TOC (`graph.rs` `profile_of`, T-086,
`receivers_follow_the_senders_frame_size_after_a_profile_switch`). Only the
encoding quality stays the old profile's until the member rejoins. A forced
rejoin would interrupt every member's call to change that. T-086's
follow-up suggested it; it stays a possible improvement, not a fault.

**Scroll after a resync.** Checked in `@tanstack/virtual-core` 3.19.11
(`anchorTo: 'end'`). When the edge keys change, the virtualizer keeps the
row being read in place if it is still in the list, and follows the end
when the reader was at the end and messages were appended
(`isAppendWithTrim`). It did nothing in two cases:
- The row being read was dropped with the older messages. The offset stayed
  where it was, over rows the reader never saw.
- A resync replaced the whole run (no message kept) and the list did not
  grow. A reader at the bottom was then left off the end, and new messages
  no longer followed.

`Chat` now notes the row being read and whether the list was at the end,
after each commit and on scroll. When the newest run changes while
`latest` holds before and after, and no jump is pending, `afterTrim`
compares the two runs. If the reading row and the old first message are
both gone, it scrolls to the end when the reader was there, or else to the
oldest kept message, with "Load older" above it. Jumps and "Jump to latest"
are excluded by the `latest` and pending-jump checks. A run that kept the
reading row is left to the virtualizer. jsdom has no layout, so the
component test replaces the virtualizer, and the real scrolling is a hands-on
row in `ACCEPTANCE.md`.

**Protocol.** No `.proto` change. `ServerView` (shell to webview) gained one
field; `src/lib/generated/ServerView.ts` was regenerated by `cargo test`.

**Files outside the Paths**
- `apps/desktop/src/lib/generated/ServerView.ts` (generated).
- `docs/plan/tasks/T-049-input-macos.md` (the focus note),
  `docs/plan/tasks/INDEX.md` (this row), `docs/plan/tasks/ACCEPTANCE.md`
  (two rows), this task file.

**Tests.** `just check` passes. `cargo clippy -p socialspeak-desktop
--all-targets -- -D warnings` and `cargo test -p socialspeak-desktop` pass.
From `apps/desktop`: `tsc -b`, `eslint .`, `prettier --check .` and
`vitest run` (28 files, 155 tests) pass. No lockfile change.

**Manual checks** (rows in `ACCEPTANCE.md`)
1. macOS, a PTT binding, Input Monitoring denied. Grant it in System
   Settings, switch back to Sideband. Shortcut settings show "Listening"
   without a restart or a second prompt.
2. An account with more than 100 DMs on one server. Scroll the DM list to
   its end; the older conversations appear and open.
3. Read older messages in a busy channel, go offline long enough for more
   than four pages to arrive, reconnect. The list lands on the oldest kept
   message; at the bottom it stays at the newest.

