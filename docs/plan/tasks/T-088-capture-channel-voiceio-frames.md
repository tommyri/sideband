# T-088 — Capture channel selection and whole-frame ring transfers

Status: awaiting-acceptance
Phase: 3
Size: S
Depends on: T-010, T-054
Paths: `crates/ss-audio/src/device/convert.rs`, `crates/ss-audio/src/device/macos_voiceio.rs`, `crates/ss-audio/src/device/stream.rs`, `apps/desktop/src/features/settings/audio/`

## Goal
A microphone on a multi-input interface is sent at full level, and stereo
channels never swap.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
measured on hardware.

## Scope
- Mono capture averages all device channels. A two-input USB interface
  with the microphone on input 1 is sent at −6 dB, an eight-input one at
  −18 dB plus the noise of the unused inputs. Take one channel (default the
  first), let the user choose it in audio settings, and keep averaging only
  as an explicit option.
- VoiceIO `pull_playback` pops sample by sample without the
  `slots() >= channels` guard the cpal path has; a producer commit landing
  during an underrun shifts the interleave by one sample and swaps left and
  right. `push_unframed` on capture can push left and fail on right when
  the ring overruns. Pop and push whole frames only.
- The AUVoiceIO input path goes through `coreaudio-rs`, whose input proc
  calls `AudioUnitGetProperty` and `Vec::resize` when the slice size
  differs from the pre-read buffer size: an allocation on the real-time
  thread on the first callback and on every size change. Pre-size from
  `kAudioUnitProperty_MaximumFramesPerSlice` or use a raw render proc.
- Reported, not measured: the worker polls `AVCaptureDevice` microphone
  mode and AU `IsRunning` on every loop iteration (about 1 kHz). Measure and
  rate-limit if it shows up.

## Out of scope
Per-channel gain or mixing matrices.

## Acceptance criteria
- Unit test: a two-channel capture with signal on channel 0 only yields
  mono at the original level.
- Property test: any interleaving of producer commits and consumer pulls
  keeps frame alignment for stereo, on both paths.
- The real-time rules in `04-conventions.md` hold for the VoiceIO callback;
  note how that was checked.

## Notes from implementation

### Findings, checked by tests written first
- Mono capture averaged every input: confirmed. cpal 0.18.2 on CoreAudio
  reports one input config with the device's total channel count
  (`host/coreaudio/macos/device.rs`, the buffer-list sum), so a mono request
  on a two-input interface opened two channels and `graph_frame` averaged
  them. `a_microphone_on_the_first_of_two_inputs_keeps_its_level` got 0.25
  for a 0.5 signal on input 1 (−6 dB).
- VoiceIO `pull_playback` popping sample by sample: confirmed. The property
  test lands producer commits between the callback's ring reads. Minimal
  case: an empty ring, one stereo callback, a commit after the first pop.
  Frame 1's left sample came out on the right, `[0, 1]`. The ring then
  stays one sample out, so the swap lasts.
- The cpal path's playback guard holds: the same property test passed on
  the old code (per-frame `slots() >= channels` check, and the worker
  commits whole quanta).
- `push_unframed` pushing left and failing on right: already fixed. T-083
  (`1a2cd53`, after the `d861a0a` review) made it push each frame with
  `push_whole`. The cpal capture property test passed on the old code.
- VoiceIO `push_capture` sample by sample: real code, latent bug. With an odd
  ring capacity the property test split a frame (capacity 47: `25, -24`).
  With even capacities (20 000 cases) it never failed: the real ring holds
  24 000 samples and the worker reads whole 960-sample quanta, so free space
  is always a multiple of two. It depended on invariants the callback does
  not own. Fixed anyway.
- coreaudio-rs input proc: confirmed by reading
  `coreaudio-rs-0.14.2/src/audio_unit/render_callback.rs` (`set_input_callback`).
  It pre-sizes from `kAudioDevicePropertyBufferFrameSize`. Then, on every
  callback whose `in_number_frames` differs from the last one, it calls
  `AudioUnitGetProperty(kAudioUnitProperty_StreamFormat)` and
  `Vec::resize`, which allocates when the slice grows. Pre-sizing alone
  would not help: the property read runs on any size change, whatever the
  capacity. Its `free_input_callback` also rebuilds the `Vec` with
  `capacity = mDataByteSize`, which no longer matches after a resize.
  So this uses a raw input proc.
- ~1 kHz polling: measured, not a problem, so no rate limit. Release build,
  this Apple Silicon MacBook Pro, a running VoiceProcessingIO unit:
  `is_running()` (AU `IsRunning`) costs 33 ns per call, worst 125 ns in
  1000. `microphone_mode_status()` (two `AVCaptureDevice` class getters)
  costs 62 ns, worst 167 ns. Neither allocates (`allocation_counter`). At
  1 kHz both together take about 0.1 ms per second, 0.01 % of a core. The
  test was temporary and is deleted.

### What changed
- `select.rs`: `CaptureChannel` (`Index(n)`, zero-based, default `FIRST`;
  or `Average`) and `GraphConfig::input` / `with_input`. `resolve` maps
  an index the device does not have to input 1. `Average` on a mono device
  is input 1. Stereo capture keeps inputs 1 and 2. Playback and
  VoiceProcessingIO ignore the field.
- `convert.rs`: capture takes a `CaptureLayout` (device channels, graph
  channels, resolved input), worked out once when the stream opens.
  `pull_playback` reads the ring's fill once and pops only whole frames of
  it; the rest of the callback is silence and one underrun. The new
  `push_frames` pushes the whole frames that fit, oldest first, and counts
  one overrun. `PlaybackRing` is the seam the property test uses
  (implemented for `rtrb::Consumer`). `model` holds the scripted ring and
  checkers that both paths' tests share.
- `stream.rs`: resolves the input on open and logs it (`input`, 1-based,
  0 = average).
- `macos_voiceio.rs`: playback and capture go through
  `convert::pull_playback` / `convert::push_frames`. Capture uses a raw
  `kAudioOutputUnitProperty_SetInputCallback` proc. Its state (`Capture`)
  is allocated before the unit. It holds a slice buffer of
  max(`kAudioUnitProperty_MaximumFramesPerSlice`, 4096) frames, read before
  and again after `initialize`, before `start`. The proc renders with
  `AudioUnitRender` into a stack `AudioBufferList` over that buffer and
  pushes. A slice over the limit, or a failed render, is counted as an
  overrun and dropped. The state is freed after the unit is disposed (field
  order in `VoiceProcessingStream`). On this Mac the unit reports 1200
  frames.
- Setting: `AudioConfig::input_channel` (ss-client-core). The worker passes
  it to every capture open, and Apply reopens when it changes.
  Desktop: `AudioPreferences.inputChannel` (1-based, 0 = average, 0–64,
  default 1; no version bump, since older records take the default).
  Audio settings › Devices has a "Microphone input" select: Input 1–16 and
  "Average of all inputs", disabled with Apple voice processing. The local
  mic test uses the setting.

### How the real-time rules were checked for the VoiceIO callbacks
- Capture: the proc body (`Capture::deliver`) is tested under
  `allocation_counter` with slices of 512, 471, 470, 1024 and 1 frames, the
  size changes that made coreaudio-rs re-read and resize: 0 allocations.
  It takes no locks and makes no property calls. `AudioUnitRender` is the
  call the input proc exists to make. Nothing is logged; counters are atomics.
- Playback: coreaudio-rs's `set_render_callback` wrapper only builds a slice
  over the unit's own buffer (read in the source, no property call or
  allocation). The body is `convert::pull_playback`, already
  allocation-tested; the VoiceIO test re-checks it under `allocation_counter`.
- On hardware: a temporary ignored test opened the unit with mono capture on
  the built-in microphone: 142 capture callbacks in 1.5 s, signal seen, ring
  filled (nobody read it). The existing ignored
  `opens_toggles_and_stops_a_real_voice_processing_unit` (three
  open/toggle/drop cycles) passes with the raw proc.

### Tests
- `convert.rs`: `a_microphone_on_the_first_of_two_inputs_keeps_its_level`
  (framed and resampled paths, input 1 and 2),
  `averaging_is_still_available_on_request`,
  `a_partial_fit_pushes_only_whole_frames`, and the proptests
  `playback_keeps_stereo_frames_whole_under_any_interleaving` (cpal: 2–4
  device channels, commits landing mid-callback) and
  `capture_keeps_stereo_frames_whole_under_any_interleaving` (framed and
  unframed, ring capacity 1–47).
- `macos_voiceio.rs`: the same two proptests on the VoiceIO functions, the
  allocation test, and `raw::tests::varying_slices_render_and_push_without_allocating`.
- `select.rs`: `a_missing_input_falls_back_to_the_first`.
- `worker.rs`: `apply_reopens_capture_on_the_chosen_input_channel` (mock
  host). It fails without the new reopen condition.
- `src-tauri/src/audio.rs`: `the_input_channel_reaches_the_device_layer`.
  Vitest: migration of `inputChannel`, and the select saving input 2.
- `just check` passes. `cargo clippy -p socialspeak-desktop --all-targets
  -- -D warnings` is clean, and so are `ss-audio` and `ss-client-core`
  without `macos-voiceio`. `cargo test -p ss-audio --features
  macos-voiceio` passes. `tsc -b`, `eslint .`, `prettier --check .` and
  `vitest run` pass in `apps/desktop`; the lockfile is unchanged.

### Files outside the Paths
- `crates/ss-audio/src/device/select.rs`: `CaptureChannel`, `GraphConfig::input`.
- `crates/ss-audio/src/device/mod.rs`: re-exports `CaptureChannel`.
- `Cargo.toml`, `crates/ss-audio/Cargo.toml`, `Cargo.lock` (one line):
  `objc2-core-audio-types` (already in the lockfile through coreaudio-rs)
  for `AudioBufferList`, and the matching `objc2-audio-toolbox` feature,
  both under `macos-voiceio`.
- `crates/ss-client-core/src/voice/mod.rs`: `AudioConfig::input_channel`.
- `crates/ss-client-core/src/voice/worker.rs`: `graph_config` takes the
  config; Apply reopens on an input change; one test.
- `crates/ss-client-core/src/voice/host.rs`: the mock records the last
  capture `GraphConfig` (test only).
- `apps/desktop/src-tauri/src/audio.rs` and
  `apps/desktop/src/lib/generated/AudioPreferences.ts` (ts-rs).
- `docs/design/voice.md`: one paragraph under Devices.

### Deviations
- `unsafe` lives in `macos_voiceio.rs` (a `raw` submodule with
  `#[allow(unsafe_code)]` and a `// SAFETY:` comment on every block), not in
  `ss_audio::platform`. It is the macOS backend's own C callback and state,
  and moving it would split the unit from the state it frees.
- The UI offers inputs 1–16 instead of the device's real count. Showing
  the real count would mean adding channel counts to `DeviceInfo` and its
  500 ms inventory diff. A device without the chosen input uses input 1.

### Open questions
- A host that offers a mono config alongside the full count (some ALSA
  `plug` devices, not CoreAudio or WASAPI shared mode) is opened in mono
  by `choose_format`, which ranks the requested channel count first. Input 2
  or Average is then silently not applied. Should `choose_format` prefer
  the full count when the selection needs it?
- Should the device list report input counts so the select shows only real
  inputs?

### Manual checks (need hardware; not done here)
macOS desktop app, a two-input USB interface (an eight-input one if at
hand) and a second client in the channel.
1. Plug a microphone into input 1 and leave input 2 empty. Audio settings:
   pick the interface, Microphone input "Input 1", Apply, Test mic. Note the
   meter level while speaking at a steady distance.
2. Pick "Average of all inputs", Apply, Test mic: the meter reads about
   6 dB lower (18 dB on an eight-input interface), with more hiss.
3. Move the microphone to input 2. "Input 2": full level. "Input 1": only
   the noise floor. The log shows `opened capture … input=2`.
4. Join voice on "Input 2" and switch to "Input 1" and back with Apply. The
   second client hears the level change, you stay in the channel, and
   capture reopens each time.
5. A mono USB microphone with "Input 2" saved: it works on input 1
   (fallback), and the log shows `input=1`.
6. Stereo through VoiceIO. Turn on Apple voice processing and use a Music
   channel. Have the second client play a left-only test tone (a
   left/right channel test track) through stereo capture. Listen on
   headphones for 10 minutes while the sender pauses and resumes every
   30 s, which makes the playback ring run dry and refill. Left stays left
   throughout. Before this change, a refill landing mid-callback could
   swap the channels for the rest of the call.
7. VoiceIO capture: during a 10-minute call, the capture overrun count in
   diagnostics stays 0. Toggle Apple voice processing off and on ten times;
   memory in Activity Monitor stays flat (the input proc state is freed
   with the unit).
8. While on VoiceIO, plug in and remove a USB headset: the unit reopens on
   the new default with no crackle or crash.

Record the results here.
