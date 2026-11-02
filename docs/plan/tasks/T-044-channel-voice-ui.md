# T-044 — Channel sidebar and voice UI

Status: in-progress
Phase: 3
Size: M
Depends on: T-042, T-041
Paths: `apps/desktop/src/features/channels/`, `apps/desktop/src/features/voice/`

## Goal
The TeamSpeak/Discord hybrid view: a channel tree where you can see who is in
each voice channel, who is talking, and join with one click.

## Scope
- Testing-app priority: deliver channel list, join/leave, members/speaking,
  mute/deafen and connection state first; advanced moderation and interaction
  polish remain in this task but do not block the test harness increment.
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
  card. Every member row and hover card renders the immutable `username`
  alongside the mutable display name so a nickname cannot impersonate an
  account.
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

Implemented the native live channel/member projection, collapsible tree, unread
and mention badges, role groups, immutable usernames, membership flags and
change-only speaking updates. Added permission-aware moderation, member moves,
persistent per-user volume/local mute, click/double-click joining, call controls,
network RTT/bitrate, and in-app shortcuts. Audio uses default OS devices and
hold-to-talk; device selection and global PTT remain T-051/T-058.

The desktop owns one native voice session. Switching channels, disconnecting,
removing a server and leaving during reconnect release it. Reconnect closes PTT;
a moderator move restarts audio muted in the destination profile. Speaking
updates poll the audio worker every 40 ms independently of diagnostic stats.

Validation:
- Workspace formatting, strict Clippy and tests passed.
- Desktop native tests cover real-server joining/switching/leaving, PTT and mute,
  persistent playback preferences, and cancelling reconnect membership.
- Frontend tests cover denied permissions, playback controls, shortcuts, and a
  React Profiler fixture with 50 channels × 30 members. A speaking update commits
  only the affected member rows instead of rebuilding the channel workspace.
- Packaged macOS smoke test against a disposable local server: login, live
  channels/role list, join, mute, deafen and leave passed; layout inspected.

Remaining acceptance (keep in-progress): measure audio-start-to-speaking-ring
latency below 100 ms with two real clients and check scrolling/interactions on
Windows, macOS and Linux at the scale fixture. The automated render-count test
is not a hardware frame-time measurement. Exercise moderator moves and network
reconnect in those installed builds. T-051/T-058/T-076 can proceed meanwhile.

Manual check: join from two computers, hold PTT and record audio/ring timing;
verify release/focus loss/mute/deafen silence transmission; test a restricted
account's disabled actions, an admin move, an outage/reconnect, and leave during
an outage. Record OS, build, devices, observed timing and results in test evidence.
