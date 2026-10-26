# T-054 — macOS "Apple voice processing" mode (AUVoiceIO)

Status: todo
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
