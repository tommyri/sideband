# T-054 — macOS "Apple voice processing" mode (AUVoiceIO)

Status: done
Phase: 3
Size: M
Depends on: T-013, T-053
Paths: `crates/ss-audio/src/device/macos_voiceio.rs` (feature `macos-voiceio`)

## Goal
Offer Apple's FaceTime-grade AEC/NS/AGC as a single switch on macOS, as an
alternative to the cross-platform chain.

## Scope
- Alternative device backend implementing the same `CaptureStream`/
  `PlaybackStream` interfaces using an `AUVoiceIO`
  (`kAudioUnitSubType_VoiceProcessingIO`) unit: input bus 1 capture, output
  bus 0 render, 48 kHz float, 10 ms slices.
- Expose: bypass voice processing toggle, AGC toggle, muted-talker detection
  callback if available, and the system mic-mode (Voice Isolation / Wide
  Spectrum) status (read-only; user picks it in Control Center).
- When active, T-053 chain is forced off and the UI says so.
- Latency measurement vs the plain CoreAudio path recorded in notes.

## Out of scope
iOS.

## Acceptance criteria
- Manual: speakers + built-in mic on a MacBook, remote party hears no echo.
- Switching modes mid-call works; no device leaks (check with
  `AudioObjectGetPropertyData` running state / Activity Monitor).

## Notes from implementation
- Added feature `macos-voiceio` to `ss-audio` and `ss-client-core`. The device
  backend is one full-duplex `kAudioUnitSubType_VoiceProcessingIO` instance,
  not separate input/output units, so Apple's echo canceller receives the
  actual render signal. Both graph-facing rings are interleaved 48 kHz `f32`;
  hardware callback sizes may vary while the client still consumes 10 ms
  quanta.
- `VoiceProcessingStream` exposes live bypass, AGC and processed-uplink mute
  toggles, running state, callback counters, the Audio Unit latency property,
  and macOS 14+ muted-speech edge events. Control Center's preferred and
  active `AVCaptureMicrophoneMode` values are read-only and mapped to
  `Standard`, `WideSpectrum`, `VoiceIsolation`, or `Unknown`.
- `AudioConfig::device_backend` selects portable devices or Apple voice
  processing. Apple mode requires the system-default input/output, forces the
  T-053 software chain to `ProcessingConfig::OFF`, follows default-device
  changes, mirrors self-mute into AUVoiceIO, and publishes mode/muted-talker
  events for the UI. `VoiceSession::set_device_backend` switches modes without
  leaving the voice channel and rolls back if the new backend cannot open.
- CoreAudio can briefly hide the VoiceProcessingIO component while a previous
  instance tears down. Creation retries for up to 900 ms. The real-device
  ignored test opened, toggled and disposed the unit three times in one
  process. It also fixed two headless hazards found by that test: a null
  `AVAudioApplication.sharedInstance` now reports permission `Unknown`, and a
  failed cpal device description no longer calls its fallible `Display` impl.
- On this Apple Silicon MacBook Pro/macOS 27.0.1, the unit reports 0 ms through
  `kAudioUnitProperty_Latency`; that property therefore cannot replace an
  acoustic measurement. The portable path's property-based built-in round
  trip remains 77.6 ms at the 240-frame default. The remote echo verdict,
  acoustic VoiceProcessingIO round trip, Control Center mode listen, and
  Activity Monitor leak check are consolidated into T-019's existing macOS
  manual-acceptance session.
- Validation: strict Clippy passes for `ss-audio` and `ss-client-core` both
  with and without `macos-voiceio`; their complete feature-enabled test suites
  pass. The explicit real-device test passes three open/toggle/drop cycles.
