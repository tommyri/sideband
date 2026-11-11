# T-102 — Notification policy moves to ss-client-core

Status: in-progress
Phase: 3
Size: M
Depends on: T-095
Paths: `crates/ss-client-core/src/notifications.rs`, `crates/ss-client-core/src/lib.rs`, `apps/desktop/src-tauri/src/notifications.rs`, `apps/desktop/src-tauri/src/persistence.rs`

## Goal
Whether a message alerts, which cue plays and what the badge counts are
decided by headless, unit-tested code that `ss-cli` could use too. The shell
only shows notifications and plays sounds.

Source: T-095. `01-architecture.md` says no business logic in `src-tauri`;
T-095 moved unread derivation, moderation rights and PTT arming and left
this because it is entangled with Tauri state.

## Scope
- Move `Preferences`/`Level` and `Preferences::allows`/`sounds` (the
  channel/server/DND policy) to a new `ss_client_core::notifications`
  module. Keep the serde shape so `sessions.json` still loads.
- Move the decision half of `notifications::observe`: mention/DM
  classification of a cached message, "own message" and visibility checks,
  the 512-entry seen-message dedupe, the per-server mention count for the
  badge, and the voice join/leave/lost/restored cue diff. Return a value
  such as `Alert { cue, desktop: Option<Body> }` that the shell renders.
- The shell keeps the notification plugin, the badge call, activation
  targets and idle cue playback.

## Out of scope
Changing what notifies. Chat behaviour (T-096).

## Acceptance criteria
- Unit tests in `ss-client-core` cover policy, dedupe, own-message and
  hidden-channel suppression, badge counts and cue selection.
- `apps/desktop/src-tauri/src/notifications.rs` holds no policy branches.

## Notes from implementation

Implemented (2026-11-11). A move with no behaviour change.

- New `ss_client_core::notifications`: `Level` and `Preferences` (same serde
  shape, now also `PartialEq`/`Eq`/`Debug`) with `allows`, `sounds`,
  `desktop_enabled` (desktop on and no DND, used by the test notification)
  and `within_limits` (the save limits: 10 000 channel levels, 1000 muted
  servers, 512-byte keys); `can_open` (connected and `VIEW_CHANNEL`, used for
  alerts, the badge and activation); `mention_count`; `presence` and
  `presence_after` (re-assert DND after a resync); `alert`, which returns
  `Alert { cue, desktop: Option<String> }` from the current cache (own
  message, hidden channel, missing message and preferences suppress it; DM
  or mention picks the cue; focus or no opt-in drops the desktop body; the
  180-character preview or the private generic text); and `Tracker`, which
  holds the 512-entry seen-message dedupe (`first_sighting`, still evicting
  the lowest `(server, id)` as before), per-server mention counts (`badge`)
  and the previous call members (`voice_cue`: join, leave, lost on the first
  reconnect attempt, restored on resync).
- The voice cue diff reads `State::voice_states` and takes the call's
  channel from the shell (`VoiceSession::channel`), so `voice/` is untouched.
- The shell keeps the notification plugin, activation targets (pruned with
  `can_open`), the badge call, presence sends and idle cue playback. Its
  remaining preference reads are not policy: `!dnd` for
  `set_cues_enabled` (as `voice.rs` does) and, on save under DND, clearing
  delivered notifications and sending DND presence. Window focus is now read
  before taking the voice lock, not after the cue plays.
- Badge counts: `unread` was not reused. The rail badge counts unread
  conversations (`unread::unread_conversations`); the dock badge counts
  server-reported mentions in channels the user can open. They are different
  numbers with one source each, so `mention_count` lives with the policy.
- Tests: 18 in `ss-client-core` (policy, limits, serde shape of a saved
  sample, mention/`@everyone`/DM cues, own/hidden/unknown/offline/missing
  suppression, edits re-read from the cache, desktop body and previews,
  dedupe across servers and the 512 cap, badge totals across servers and on
  disconnect, live mention counts, the voice cue diff, lost/restored and
  silencing). The desktop's policy test moved there; the shell keeps the
  restart test and `persistence` gains a test that loads a saved
  `sessions.json` sample in the current shape and writes it back unchanged
  (written and passing before the move).

Files outside Paths: none besides the task file and `INDEX.md`.
