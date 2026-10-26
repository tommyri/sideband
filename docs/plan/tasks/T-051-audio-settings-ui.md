# T-051 — Audio and voice settings UI

Status: todo
Phase: 3
Size: M
Depends on: T-044, T-041, T-047
Paths: `apps/desktop/src/features/settings/audio/`

## Goal
Everything a TeamSpeak user expects to tune, laid out so a non-expert gets
good results with defaults.

## Scope
- Device pickers (input/output) with "System default" option, live input
  level meter, "Test mic" loopback (hear yourself after 1 s delay), "Test
  output" cue.
- Transmission mode: Push-to-talk (bind key via capture, release delay
  slider), Voice activation (threshold slider with live meter and auto-
  calibrate button, hangover), Open mic.
- PTT platform notice from `ss-input` `Caps`: permission button (macOS),
  "set in system settings" (Wayland portal), limitations text, evdev opt-in
  toggle with warning (Linux).
- Processing toggles (all off by default, each with one-line explanation and
  CPU/latency cost): echo cancellation, noise suppression (model picker),
  automatic gain control; macOS: "Use Apple voice processing" mode switch
  (T-054) that disables the others.
- Output: master volume, per-user volumes list (reset all), cue volume,
  attenuation of other users while you speak (0–50 %).
- Advanced: show profile/bitrate of current channel (read-only), jitter
  buffer min/max, "prefer low latency (10 ms frames)" toggle if T-018 keeps
  it, diagnostics link (T-063).
- Settings persisted via `tauri-plugin-store`; applied live to the
  `VoiceSession`.

## Out of scope
Server-side channel settings (T-056).

## Acceptance criteria
- Changing devices mid-call takes effect within 1 s without leaving the
  channel.
- PTT binding capture works for keyboard and mouse on platforms that support
  it; the UI never offers an option the backend reports as unsupported.
- Vitest tests for settings store migrations/defaults.

## Notes from implementation
