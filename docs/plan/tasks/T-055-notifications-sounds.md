# T-055 — Notifications, sound cues, do-not-disturb

Status: in-progress
Phase: 3
Size: S
Depends on: T-042, T-041
Paths: `apps/desktop/src/features/notifications/`, `apps/desktop/src-tauri/src/notifications.rs`, `crates/ss-client-core/src/voice/`

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

- Implemented native desktop notifications (explicit opt-in, private generic bodies
  by default, optional text previews), notification-click focus/message navigation,
  per-channel and DM all/mentions/none overrides, server mute, persisted DND and
  presence synchronization, and unread mention badges on supported platforms.
- Only newly received live message IDs trigger alerts; cached echoes, snapshot
  overlap, history hydration, own messages, inaccessible channels, and queued
  messages deleted before observation are suppressed. Current edited content and
  mention policy are re-read from the authoritative cache. Message text never
  enters the generic frontend event/diagnostic stream or saved preferences.
- Self-made 60 ms, 48 kHz stereo PCM cues are synthesized directly rather than
  shipping an Opus asset pack. The existing native voice mixer injects join/leave,
  mute/unmute/deafen, mention/DM and connection cues. Message alerts outside calls
  use output-only ss-audio playback with the same PCM and saved output/master/cue
  gain; this never opens microphone capture. Bursts are bounded/coalesced. DND
  clears queued voice cues, and per-server voice mute does not silence policy-
  approved messages from another server.
- Bell button on the server rail opens notification preferences. The explicit
  Save and send test action is the only permission-request path; opening the app
  or preferences does not request permission or send a test notification. The
  desktop plugin reports permission as granted; OS settings remain authoritative.
- Automated coverage: Rust live-message dedup/snapshot overlap, cue queue bounds,
  DND clearing and cue gains; native policy and persistence; Vitest preference
  precedence, explicit save/opt-in, and errors. Rendered UI checked with a mocked
  native bridge at 1200×900 and 760×600, including server/channel overrides.

### Remaining installed-app acceptance (keep task in progress)

1. On Windows, macOS and Linux, enable notifications, background/minimize the app,
   send a mention and DM from another account, and click each notification. Verify
   window focus, correct server/channel/message navigation and read-count badges.
2. Repeat with DND, server mute and each channel level, reconnect, revoked access,
   edits/deletes, and multiple connected servers (including a muted active call).
3. Listen to every cue, including a DM outside voice, selected output device,
   master/cue volume zero, deafening and lost/restored connection. Measure trigger
   to audible onset against the <50 ms target. Output-only device opening is not
   prewarmed and may exceed this target; hardware evidence is needed before claiming
   latency acceptance or deciding whether a persistent idle output stream is needed.
4. Plugin 2.8 limitations: Windows click callbacks only while a toast is visible,
   not after it moves to Action Center; macOS/Windows cannot retract delivered
   notifications. Click targets are invalidated/rechecked locally after access loss
   or disconnect, but previously displayed OS previews cannot be recalled there.
   Leave previews off for sensitive conversations. Linux action/badge support
   varies with the desktop notification service. No cold-start click persistence.

Optional spoken joiner names remain backlog only.
