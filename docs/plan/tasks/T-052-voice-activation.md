# T-052 — Voice activation detection

Status: todo
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
