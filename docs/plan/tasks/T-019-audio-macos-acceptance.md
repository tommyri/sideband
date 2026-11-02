# T-019 — macOS audio: manual acceptance and timestamp correction

Status: todo
Phase: 1
Size: S
Depends on: T-013, T-054
Paths: `docs/plan/notes/audio-macos.md`, `crates/ss-audio/src/device/`, `crates/ss-audio/src/platform/macos.rs`

## Goal
Close the checks T-013 could not run unattended, then correct the macOS
latency timestamps if the acoustic numbers confirm the error T-013 found.

## Scope
- Acoustic round trip on the built-in devices with system output unmuted,
  at buffer sizes 128, 240, and 256, using the click-correlation method in
  `docs/plan/notes/audio-macos.md`. Compare with the 73–78 ms that CoreAudio's
  latency properties predict.
- AirPods connect and disconnect while
  `cargo run -p ss-audio --example loopback -- 180` runs, wearing headphones.
  Expect `DefaultChanged` events, printed reopens (including the low-quality
  16/24 kHz microphone warning), and no exit. Switching input to the iPhone
  Continuity microphone exercises the same path.
- A USB interface: granted rate and buffer size, resampler on or off, memory
  flat over five minutes if it is 44.1 kHz-only, then unplug while running.
- A stored "Don't Allow" microphone decision in a real app bundle yields
  `device::Error::PermissionDenied` (an ad-hoc signed bundle is enough until
  T-057 exists).
- Five-minute glitch listen at 240 frames on headphones (carried over from
  T-010).
- With a remote listener and built-in speakers/microphone, switch an active
  `VoiceSession` from `DeviceBackend::Portable` to
  `AppleVoiceProcessing` and back. Confirm the remote party hears no echo,
  record the acoustic mouth-to-ear result for both paths, and verify Voice
  Isolation/Wide Spectrum status follows Control Center. Repeat switches for
  five minutes and confirm Activity Monitor and CoreAudio running state stay
  flat after leaving.
- If the acoustic numbers confirm it, correct `cpal`'s macOS capture and
  playback timestamps by `kAudioStreamPropertyLatency` (about 50 ms on the
  MacBook microphone, 14 ms on its speakers) and remove the double-counted
  buffer, so `capture_delay` and `playback_delay` match reality. Re-run the
  T-017 mouth-to-ear measurement, which currently reads about 50 ms low.

## Out of scope
The CoreAudio voice-activity property (backlog).

## Acceptance criteria
- `docs/plan/notes/audio-macos.md` gains the acoustic measurements and the
  outcome of every manual check above.
- Decision on the timestamp correction recorded. If applied, the reported
  round trip is within 5 ms of the acoustic measurement on the built-ins.

## Notes from implementation
