# T-013 — macOS audio path verification (CoreAudio, buffer size, permissions)

Status: done
Phase: 1
Size: S
Depends on: T-010
Paths: `crates/ss-audio/src/device/macos.rs` (only if needed), `docs/plan/notes/audio-macos.md`

## Goal
Confirm the macOS path is low-latency and handles microphone permission and
device switching (AirPods, USB interfaces) correctly.

## Scope
- Measure round-trip latency with built-in mic/speakers and a USB/Bluetooth
  headset; try `kAudioDevicePropertyBufferFrameSize` 128/256 via cpal buffer
  size request.
- Microphone permission: ensure `NSMicrophoneUsageDescription` is documented
  for the Tauri bundle (T-057) and that a denied permission yields a clear
  `DeviceError::PermissionDenied`.
- Default device change (AirPods connect) → `DeviceEvent::DefaultChanged`
  and reopen.
- Evaluate `kAudioDevicePropertyVoiceActivityDetectionEnable` availability
  (macOS 14+/15+) as a future VAD source; document only.
- Sample-rate mismatch handling (device at 44.1 kHz) through the T-010
  resampler.

## Out of scope
`AUVoiceIO` processing mode (T-054).

## Acceptance criteria
- `docs/plan/notes/audio-macos.md` with measurements and decisions.
- Loopback example survives AirPods connect/disconnect without restart.

## Notes from implementation

Full results: [`docs/plan/notes/audio-macos.md`](../notes/audio-macos.md).

- Measured on an Apple Silicon MacBook Pro, macOS 27.0.1, `cpal` 0.18.2,
  with three sibling builds running (load 8 to 12). CoreAudio granted every
  `BufferSize::Fixed` request exactly (128, 240, 256, 480; range 15 to
  4,096), with no xruns. The reported round trip on the built-in devices is
  2 × buffer + 3,247 frames: 73 ms at 128, 77.6 ms at ss-audio's 240-frame
  request, and 78.3 ms at 256. Most of it is the built-in microphone's
  50 ms stream latency. The 5 ms request stays.
- The acoustic round trip was not measured because the system output was
  muted. The click-correlation method was checked on a virtual loopback
  device. `cpal`'s macOS timestamps count the buffer twice and leave out
  `kAudioStreamPropertyLatency` (about 64 ms on the built-ins). The fix is
  in `BACKLOG.md` and waits for the acoustic check.
- Fixed: `RateConverter` grew its output buffer without bound when a
  caller pulled whole frames (192 KB/s at 44.1 kHz mono). A device that
  offers 48 kHz, as the built-ins do, is switched to 48 kHz by `cpal`
  instead of being resampled.
- Added: a denied microphone opens and delivers zeros in CoreAudio, with
  no error. `open_capture` now reads `AVAudioApplication.recordPermission`
  (macOS 14 and newer) and returns `Error::PermissionDenied`. A capture
  that is still all zeros after one second emits
  `DeviceEvent::SilentInput { id, access }`. This was verified with an app
  bundle without `NSMicrophoneUsageDescription`. The `unsafe` AVFAudio calls
  live in `src/platform/macos.rs`, the crate's existing unsafe-allowed
  platform module, rather than `src/device/macos.rs`.
- Added: `DeviceEvent::requires_reopen`. A capture opened on the default
  does not follow a new default in CoreAudio; output does. The loopback
  example now reopens on `Lost` and on `DefaultChanged` instead of exiting.
  The tests replay an AirPods connect and disconnect from inventory
  snapshots. `ss-cli` still reopens only on `Lost`, which is left to T-041.
- Still manual: the acoustic round trip, AirPods connect and disconnect,
  a USB interface (and a 44.1 kHz-only device), a stored "Don't Allow"
  in a real bundle, and the five-minute listen. The note lists the steps.
