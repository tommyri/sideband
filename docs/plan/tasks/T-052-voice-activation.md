# T-052 — Voice activation detection

Status: done
Phase: 3
Size: S
Depends on: T-010, T-014
Paths: `crates/ss-audio/src/vad.rs`

## Goal
A VAD good enough that open-mic users don't transmit keyboard noise or
breathing, with no added latency.

## Scope
- Baseline: RMS/peak energy gate in dBFS with user threshold, attack 10 ms,
  hangover 300 ms (configurable), pre-roll buffer of 2 frames so word onsets
  aren't clipped (send them when the gate opens).
- Better: optional RNNoise-derived VAD probability (RNNoise exposes one; only
  when the `rnnoise` feature is on) or Silero-style tiny ONNX model behind
  `vad-ml` feature; combine with energy gate (`prob > p && level > t`).
- Auto-calibrate: measure noise floor for 2 s, set threshold +12 dB.
- Emits `GateEvent::{Open, Close}` to `TalkGate`; exposes current level and
  probability for the UI meter at 50 Hz.

## Out of scope
Noise suppression itself (T-053).

## Acceptance criteria
- Test fixtures: speech+keyboard WAV → the gate stays closed during
  keyboard-only segments and opens within 20 ms of speech onset (energy
  mode); ML mode strictly better on the same fixture.
- Zero allocation per frame.

## Notes from implementation
- Implemented on `task/T-052-voice-activation`; awaiting review and merge.
- `vad::Vad` accepts exact 10 ms, 48 kHz normalized mono/stereo quanta on
  the capture worker. Energy uses the louder channel's RMS, avoiding stereo
  phase cancellation. One qualifying quantum opens the gate; default quiet
  hangover is 300 ms, configurable from 0 to 5 s in 10 ms increments.
- `Output::audio()` releases up to two unsent pre-roll quanta followed by the
  current quantum. Feed them once, in order. Handle Open before this audio
  and Close after it; a closing output may contain the final hangover frame.
  Already-sent tail frames are not repeated on a quick reopening. Force-close
  clears pre-roll for mute/pause. Session/TalkGate wiring belongs to T-041.
- Two-second calibration suppresses transmission and uses median frame
  energy +12 dB (clamped to -96..=0 dBFS). The 50 Hz meter supplies RMS, peak,
  threshold, and optional probability even when the gate is closed.
- Optional `rnnoise` uses pinned `nnnoiseless` 0.5.2, default features off,
  bundled model, one state per channel. It requires both energy and speech
  probability (default 0.6); denoised output is discarded. Construction must
  happen on the processing thread to warm the dependency's thread-local FFT
  caches. Processing and calibration allocate zero bytes after construction,
  including the first non-silent quantum; tests cover mono and stereo.
- The credited WAV fixture combines a VOiCES speech excerpt with deterministic
  synthetic keyboard clicks. At normal gain, energy mode opens on the first
  speech quantum (10 ms) and stays shut during keyboard-only regions outside
  hangover. At 4x gain on the identical fixture, false-active keyboard quanta
  are 370 for energy and 0 for RNNoise. RNNoise retains 198/200 speech quanta;
  its two-quantum delay is covered by unchanged pre-roll PCM. This is a small
  regression fixture, not evidence of general keyboard/breathing immunity.
- Tests cover exact attack/tail timing, retriggering, PCM preservation,
  stereo cancellation, invalid input without state changes, calibration,
  50 Hz metering, zero allocations, and comparative fixture behavior. CI
  now tests and lints the optional RNNoise feature on all three platforms.
- Dependency audit also exposes existing baseline issues unrelated to T-052:
  `audio_thread_priority` MPL-2.0 is outside the allow-list; `h2` 0.4.13 has
  RUSTSEC-2026-0258; `rustls-pemfile` 2.2.0 has RUSTSEC-2025-0134. No versions
  or license policy for those dependencies were changed in this task.
- Validation: `just check` passed (261 tests passed, 1 manual load test ignored);
  `just check-deps` passed; `cargo test -p ss-audio --features rnnoise` passed
  (49 tests, including 12 VAD tests); optional-feature Clippy and
  `git diff --check` passed. `cargo deny --all-features check` reports only
  the three baseline issues listed above; no new dependency failures.
