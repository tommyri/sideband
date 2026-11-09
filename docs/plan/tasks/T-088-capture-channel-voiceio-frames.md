# T-088 — Capture channel selection and whole-frame ring transfers

Status: todo
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
