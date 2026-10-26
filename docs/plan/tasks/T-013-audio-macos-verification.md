# T-013 — macOS audio path verification (CoreAudio, buffer size, permissions)

Status: todo
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
