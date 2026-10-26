# T-053 — Optional audio processing chain: AEC, noise suppression, AGC

Status: todo
Phase: 3
Size: M
Depends on: T-010
Paths: `crates/ss-audio/src/processing/`

## Goal
Opt-in processing for people on speakers or in noisy rooms, with zero effect
on users who leave it off.

## Scope
- `Processor` trait: `process(capture_frame_10ms, render_reference_10ms)`;
  chain order AEC → NS → AGC; bypass when all disabled (no copy).
- AEC + AGC: `sonora` (pure Rust port of WebRTC APM: AEC3, AGC2, HPF) behind
  feature `aec`; render reference tapped from the mixer output *before* the
  playback ring, with delay estimation from measured device latencies
  (T-010 timestamps).
- Noise suppression options: `rnnoise` (feature `rnnoise`, via `nnnoiseless`
  pure-Rust port or C bindings), `deepfilter` (feature `deepfilter`,
  DeepFilterNet3 via `deep_filter`/`df` crate or ONNX `_ll` model; 10 ms
  frames); model files downloaded at build or bundled (size budget ≤ 10 MB).
- Each stage reports added latency and CPU per frame for the settings UI.
- Processing runs in the capture worker thread, never in the device
  callback; it must finish well under 10 ms per frame (budget 3 ms).

## Out of scope
Apple `AUVoiceIO` mode (T-054), GTCRN (backlog with a note that it's the
best quality/CPU candidate once a Rust-friendly runtime path exists).

## Acceptance criteria
- Fixture tests: speech + injected far-end echo → AEC reduces echo by
  ≥ 20 dB ERLE; speech + fan noise → NS improves SNR without > 1 dB speech
  loss (measure with simple SNR on known fixtures).
- Benchmarks show each stage < 1.5 ms per 10 ms frame on CI runners.
- With all stages disabled, the capture path is byte-identical to T-010
  output.

## Notes from implementation
