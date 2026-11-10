# T-102 — Notification policy moves to ss-client-core

Status: todo
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
