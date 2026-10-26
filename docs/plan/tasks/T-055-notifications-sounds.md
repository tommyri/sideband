# T-055 — Notifications, sound cues, do-not-disturb

Status: todo
Phase: 3
Size: S
Depends on: T-042, T-041
Paths: `apps/desktop/src/features/notifications/`, `crates/ss-audio/assets/cues/`

## Goal
You know when someone joins your channel or mentions you, even while gaming,
without it being annoying.

## Scope
- Sound cue pack (CC0 or self-made, short, 48 kHz Opus): user joined/left
  your channel, you joined/left, mention, DM received, mute/unmute, deafen,
  connection lost/restored. Played through the T-041 cue injection (so they
  obey the output device and cue volume), not the webview.
- Desktop notifications (`tauri-plugin-notification`): mentions and DMs
  when the window isn't focused; click focuses the channel.
- Per-channel notification level (all / mentions / none) persisted locally;
  server-wide mute; DND status suppresses sounds and toasts (presence
  `dnd` set via T-024 `SetPresence`).
- Optional TTS-style announcement of joiners' names (backlog note only).
- Taskbar/dock badge with unread mention count where supported.

## Out of scope
Mobile push.

## Acceptance criteria
- Cues play with < 50 ms latency after the triggering event.
- Notification click deep-links to the channel/message.
- Vitest tests for the notification-level resolution logic.

## Notes from implementation
