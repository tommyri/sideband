# T-014 — `ss-audio`: Opus encoder/decoder wrapper (libopus 1.6, Deep PLC, DRED flag)

Status: done
Phase: 1
Size: S
Depends on: T-003
Paths: `crates/ss-audio/src/codec/`

## Goal
A safe, allocation-free-at-runtime Opus wrapper configured from
`VoiceProfile`, with Deep PLC on and DRED optional.

## Scope
- Dependency `opusic-c` (bundled libopus 1.6.x). Features: `dred` → enable
  libopus DRED/OSCE build flags.
- `Encoder::new(profile: VoiceProfile) -> Encoder`: applies
  `VoiceProfile::encoder_params()` as specified in `docs/design/voice.md`
  (application, bitrate, channels, frame, `OPUS_SET_INBAND_FEC(1)`,
  `OPUS_SET_PACKET_LOSS_PERC` only when the profile sets a percent — 5 for
  `voice` — complexity 10, VBR on, DTX off); `set_bitrate()`,
  `set_packet_loss()` for adaptation; `encode(&[f32; frame]) -> &[u8]` into a
  preallocated buffer.
  With `dred`: `set_dred_duration_ms(0..=1000)`.
- `Decoder::new(channels)`: `decode(packet: Option<&[u8]>, fec: bool) ->
  &[f32]`; `None` triggers PLC (Deep PLC when available). With `dred`:
  `decode_dred(packet, lost_frames)` path.
- `OpusPacket::toc()` helpers: frame duration, channel count, to validate
  incoming packets cheaply before buffering.
- Bench (`criterion`): encode+decode per 20 ms frame on CI machines; assert
  well under 1 ms.

## Out of scope
Jitter buffer, networking.

## Acceptance criteria
- Round-trip test: 1 kHz sine at −12 dBFS through voice profile → SNR
  ≥ 20 dB after 100 ms warm-up; music profile stereo preserves channel
  separation.
- Loss test: drop every 10th packet; decode with FEC produces no silence gaps
  longer than one frame.
- `dred` feature compiles and a decode-with-DRED test passes when enabled; the
  default build excludes it.

## Notes / references
- Encoder contract and why DRED stays behind a flag: `docs/design/voice.md`.
- Opus 1.5/1.6 release notes; DRED is experimental (IETF draft, extension id
  may change).

## Notes from implementation

Implemented and reviewed on `task/T-014-opus-codec`; merged locally to `main`.

- `codec::{Encoder, Decoder, OpusPacket}` enforce profile bitrates, finite PCM,
  fixed 10/20 ms stream duration, and the 1150-byte media payload limit.
  `Decoder::for_profile` sets the correct PLC duration before the first packet;
  `Decoder::new(channels)` defaults to 20 ms. Channel conversion remains Opus's
  normal behavior (a stereo stream may contain mono-coded packets).
- Pinned `opusic-c` 1.6.1 / locked `opusic-sys` 0.7.6 bundle libopus 1.6.1.
  Upstream's normal build omits Deep PLC, so OSCE is enabled by default to
  compile it. Complexity 10 activates Deep PLC and low-rate LACE/NoLACE;
  BWE is explicitly disabled. DRED is compiled only with `ss-audio/dred`
  and starts disabled at runtime. Generated C compiler flags were checked
  for `ENABLE_DEEP_PLC` in both builds and `ENABLE_DRED` only in the opt-in build.
- The private `codec::native` exception supplies decoder complexity without
  DRED, full framing validation, and chronological DRED offsets. It uses
  libopus's own allocation/destruction functions and documented SAFETY blocks.
  The high-level binding gates decoder complexity behind DRED and cannot
  select arbitrary recovery offsets; it also hardcodes its DRED storage size.
- Recovery handles multiple frames in chronological order and returns
  `DredUnavailable` before advancing audio state when history is insufficient.
  Duration is an upper bound: actual history depends on bitrate, expected loss,
  voice activity, and packet budget. Parsing requests an extra 40 ms block to
  compensate for libopus 1.6.1's timestamp-offset/latent-budget rounding.
- Sine quality is **27.56 dB phase-aligned tone SNR** after 100 ms warm-up.
  Phase is fitted per 20 ms window, bounded in absolute correction and
  consecutive-window change to four samples (0.083 ms). The original amplitude stays fixed, so
  gain error and distortion count as noise. A single lookahead-only waveform
  comparison gives 11.33 dB because the SILK/CELT filter phase changes during
  the tone. The test explicitly measures tone quality, not waveform identity.
- Stereo separation, every-tenth-packet loss/FEC, initial PLC for all profiles,
  invalid input, and arbitrary packet framing are covered. With DRED, tests
  recover 3- and 10-frame bursts for all profiles, including stereo and 200 ms
  recovery. Streaming tests count zero Rust
  heap allocations (the counter does not instrument native C allocation).
- On this Apple Silicon machine, default-feature release encode+decode means:
  voice 36.5 µs, music 78.1 µs, low latency 18.9 µs. Criterion asserts the
  mean below 1 ms; CI runs it alongside default and optional-DRED tests/lints.
- Validation: `just check` passed (122 tests, one manual test ignored);
  optional-DRED tests (11), optional-feature clippy, release benchmarks, and
  `just check-deps` passed locally on macOS. Linux/Windows remain CI coverage.
- Native builds now require CMake plus a C/C++ compiler; README and CI list
  the prerequisite. The server dependency graph still excludes Opus.

## Review fixes

- Standards: pinned the default macOS deployment target to 13.0 for both Rust
  and bundled C/C++ dependencies. Rebuilt native Opus objects after clearing
  the dependency's cached build; the binding does not track this environment
  variable for rebuilds. Explicit caller deployment-target overrides remain possible.
- Spec: constrained the tone test's phase correction and continuity so arbitrary
  phase jumps and polarity inversions cannot pass. Kept the raw waveform SNR
  as a separate printed diagnostic instead of hiding its difference.
- Benchmark now uses independent stereo tones to exercise the music path.
- Both review axes have no remaining actionable findings after these fixes.
- Dependency audit exposed a pre-existing CI license rejection for
  `webpki-roots` 1.0.11 (already on main). Added a crate-scoped allowance for
  its CDLA-Permissive-2.0 trust-store data license; its LICENSE must accompany
  redistributed data. `cargo deny check` passes with duplicate-version warnings.
