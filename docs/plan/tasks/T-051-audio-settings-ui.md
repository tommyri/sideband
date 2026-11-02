# T-051 — Audio and voice settings UI

Status: in-progress
Phase: 3
Size: M
Depends on: T-044, T-041, T-047
Paths: `apps/desktop/src/features/settings/audio/`

## Goal
Everything a TeamSpeak user expects to tune, laid out so a non-expert gets
good results with defaults.

## Scope
- Testing-app priority: input/output selection, PTT vs VAD and honest
  permission/backend notices, integrated with T-058. Do not wait for the
  T-018 release verdict to expose currently supported voice settings.
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

Implemented the audio settings dialog from onboarding, the channel sidebar and
call controls. Preferences are versioned in `audio.json` using tauri-plugin-store;
missing fields inherit safe defaults and invalid settings are rejected natively.
Apply changes the running VoiceSession without leaving its channel. Device-open
failures restore the prior configuration; disk-write failures do not apply new
settings. Moderator moves reuse saved settings and start muted.

Available controls: system/default input and output, PTT/VAD/open microphone,
release delay, VAD threshold/hangover, local noise-floor calibration, in-app
keyboard/mouse PTT capture (subject to backend capabilities), master/cue/per-user
volume, reset per-user volumes, speaking attenuation and jitter min/max.
Software AEC/WebRTC NS/AGC and RNNoise are linked in desktop builds but off by
default; Apple VoiceProcessingIO is available on macOS and excludes software
processing and explicit devices. Controls report actual compiled capabilities.

Local tests: 10-second raw mic loopback with a 1-second delay, a 1-second output
cue, and 2-second median noise-floor calibration. Tests cannot run during a call,
never connect to a server, are cancellable, and stop when the dialog closes.
The input meter runs separately from the form so meter updates do not rebuild it.
Use headphones for loopback. A test completion is not proof the output was heard.

Validation:
- Vitest defaults/migrations, unsupported choices, native apply/error handling,
  Apple exclusions, calibration drafts and keyboard capture tests.
- Native tests apply live settings while retaining channel membership, reject
  invalid settings, preserve mute, and verify jitter bounds and playback gain.
- Packaged macOS UI checked for device/capability reporting, keyboard capture,
  Apply, output-test completion and settings persistence across restart; native focus/layout issues fixed.

Remaining acceptance: measure device-switch latency under 1 second on Windows,
macOS and Linux with real input/output hardware, listen to loopback and cues,
and exercise permission-denial, unplug/replug and Apple processing. No microphone
permission was granted by automation. T-058 still owns the global listener,
macOS Input Monitoring permission action and Linux evdev opt-in integration;
this screen explains that limitation and only starts focused in-app bindings.
T-063 diagnostics stays visibly unavailable and the optional 10 ms override
remains gated by the T-018 release verdict. These do not block T-076 increments.
