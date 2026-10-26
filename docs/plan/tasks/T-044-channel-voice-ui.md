# T-044 — Channel sidebar and voice UI

Status: todo
Phase: 3
Size: M
Depends on: T-042, T-041
Paths: `apps/desktop/src/features/channels/`, `apps/desktop/src/features/voice/`

## Goal
The TeamSpeak/Discord hybrid view: a channel tree where you can see who is in
each voice channel, who is talking, and join with one click.

## Scope
- Tree: categories collapsible, text channels with unread/mention badges,
  voice channels listing members (avatar, name, mute/deaf/server-mute icons,
  speaking ring driven by TALK flags/arrival), user count/limit.
- Join on click (double-click option in settings), drag a member to move
  (requires `MOVE_MEMBERS`), context menus permission-aware: mute/deafen self,
  server mute, move to…, kick, ban, set volume (slider 0–200 %), local mute,
  copy user id.
- Bottom "self" panel: avatar, name, connection quality dot (from
  `VoiceStats`), mute/deafen toggles, settings gear, "disconnect".
- Voice channel header when joined: channel name, profile (voice/music),
  bitrate, latency, leave button.
- Member list pane (right): grouped by role with colors, presence, hover
  card.
- Keyboard shortcuts in-app (not global): mute, deafen, channel up/down.

## Out of scope
Global hotkeys (T-058), text chat (T-045), admin dialogs (T-056).

## Acceptance criteria
- Speaking indicator latency from audio start < 100 ms (visual check).
- Rendering 50 channels × 30 members stays smooth (React profiler, no
  re-render storms: memoized rows).
- All actions degrade correctly without permission (hidden or disabled with
  tooltip).

## Notes from implementation
