# T-053 — Optional audio processing chain: AEC, noise suppression, AGC

Status: done
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
- Implemented on `task/T-053-audio-processing-chain` and integrated into the
  current task lineage.
- `ss_audio::processing`: `Processor` trait (`process(capture, render)` on
  one interleaved 10 ms, 48 kHz frame, plus stage, backend, latency, echo
  delay hint, reset) and `Chain`, which builds AEC → NS → AGC from a
  `ProcessingConfig` (default all off). `render` is a slice of the far-end
  mixer frames that arrived since the previous call, usually one, because
  playback and capture clocks differ. `Chain::with_processors` accepts
  custom stages in that order. Invalid lengths and non-finite PCM are
  rejected with the frame untouched.
- Bypass: with no stage, `Chain::process` checks the length and returns
  without reading or writing a sample. A test runs random frames (including
  NaN bit patterns, -0.0, and subnormals) through mono and stereo bypass and
  compares `to_bits`, with zero allocations.
- Render reference: `render_tap` is an `rtrb` queue of `StereoFrame`s. The
  playback worker pushes each mixer frame before the playback ring; the
  capture worker drains up to 8 frames per capture frame (older excess is
  discarded and counted). `EchoPath` sums playback queue, `playback_delay`,
  `capture_delay`, and capture queue into the AEC3 delay hint. Wiring the
  tap and the chain into the session's capture worker belongs to T-041.
- Real backends: `aec` uses `sonora` 0.2.0 (pinned, pure Rust, BSD-3) for
  AEC3 (with its enforced high-pass filter), the WebRTC suppressor as an
  extra NS choice, and AGC2 adaptive digital gain plus limiter. Each stage is
  its own `AudioProcessing` instance at the 48 kHz internal rate (sonora
  defaults to 32 kHz, which would drop 16–24 kHz). `rnnoise` reuses T-052's
  exact `nnnoiseless =0.5.2` workspace entry, feature name, and comment, one
  state per channel, warmed at construction. The comment says "speech
  probability"; after both branches merge it should read "denoise and
  speech probability".
- `deepfilter` is a documented stub: the feature compiles nothing and
  `NoiseSuppression::DeepFilter` returns `Error::Unavailable` (or
  `FeatureDisabled` without the feature). The crates.io `deep_filter`
  0.2.5 has no inference; the inference build (`deep_filter` 0.5.7-pre,
  `tract` feature) exists only in git, which `deny.toml` rejects
  (`unknown-git = "deny"`), and its tract dependency compiles assembly
  kernels with `cc`. `DeepFilterNet3_ll_onnx.tar.gz` is 36.4 MB, over the
  10 MB budget; the 8.0 MB standard model has two frames of look-ahead,
  contrary to the low-latency design. Other crates were rejected: `anr`
  needs ONNX Runtime or a GPU backend; `hush-vani` is a different model
  (Hush) at 16 kHz. Backlog entry added; GTCRN entry now notes it is the
  best quality/CPU candidate.
- Latency per stage, measured as the impulse-response peak and asserted to
  ±2 samples: AEC3 238 samples (4.96 ms: 4 ms block framing plus the
  three-band split), WebRTC NS 334 (6.96 ms), AGC2 0, RNNoise 480 (10 ms).
  AEC3 + RNNoise + AGC2 measures 718. Separate sonora instances pay the band
  split per instance; a fused sonora AEC+NS would save about 1 ms.
- `Monitor` (clonable, lock-free atomics) reports per stage: stage, backend,
  latency, and CPU last/mean/max per frame; also whole-chain CPU and frames
  over the 3 ms `CHAIN_BUDGET`.
- Fixtures are synthetic and deterministic (`tests/processing/signals.rs`):
  formant speech with pitch movement, fricatives, and pauses; fan noise
  (pink, 87 Hz blade harmonics, rumble); a 40 ms bulk-delay room echo with
  12 reflections and a speaker low-pass.
  - AEC3, far-end single talk after 2 s: ERLE 56.2 dB with a matching 40 ms
    hint, 44.6 dB with a 10 ms hint (asserted ≥ 20 dB). Near-end-only loss
    after the far end stops: 1.07 dB (asserted < 3 dB).
  - NS at 11.4 dB input SNR (frame-based simple SNR: active frames within
    15 dB of speech level vs speech-free frames): RNNoise 11.4 → 31.0 dB,
    speech loss 0.37 dB (asserted ≥ 10 dB gain, ≤ 1 dB loss). The WebRTC
    suppressor (moderate) reaches 25.3 dB but loses 1.08 dB of speech, so it
    is asserted at ≤ 1.5 dB and documented as second choice; RNNoise is the
    recommended suppressor.
  - AGC2 raises a −50 dBFS talker by 28.6 dB, a −26 dBFS talker by 6.2 dB,
    and keeps peaks at or below full scale.
- Benchmark (`cargo bench -p ss-audio --all-features --bench processing`,
  release, this M-series Mac under four concurrent builds), mean per 10 ms
  frame mono / stereo: AEC3 46 / 58 µs, WebRTC NS 6 / 12 µs, AGC2 15 /
  16 µs, RNNoise 31 / 62 µs, AEC3 + RNNoise + AGC2 92 / 136 µs. The bench
  asserts < 1.5 ms per stage and < 3 ms per chain; CI now runs it plus
  all-feature tests and Clippy on every OS.
- Allocation: RNNoise and the WebRTC suppressor allocate nothing per frame
  (tested). sonora's AEC3 (~44 allocations, 15 KB per mono frame) and AGC2
  (~11, 6 KB) allocate inside the dependency every frame. That is allowed on
  the capture worker but would not be in a device callback; worth an
  upstream issue.
- Manual check still needed (with T-041; a real `deepfilter` backend is T-059): speakers plus microphone with a real far end
  once T-041 wires the tap, to confirm the device-delay hint and acoustic
  ERLE; listening tests of RNNoise vs the WebRTC suppressor on real speech.
- Validation: `just check` passed (all workspace tests, one existing manual
  load test ignored); `just check-deps` passed; `cargo test -p ss-audio
  --all-features` and `cargo clippy -p ss-audio --all-targets --all-features
  -- -D warnings` passed, as did Clippy and tests per single feature;
  `git diff --check` clean. `cargo deny --all-features check` reports only
  the three baseline issues T-052 recorded (`audio_thread_priority` MPL-2.0,
  `h2` RUSTSEC-2026-0258, `rustls-pemfile` unmaintained); `sonora` and its
  sub-crates are BSD-3-Clause, `nnnoiseless` BSD-3-Clause.
