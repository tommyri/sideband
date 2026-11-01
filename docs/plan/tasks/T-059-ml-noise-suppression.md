# T-059 — ML noise suppression behind the `deepfilter` feature

Status: blocked
Phase: 3
Size: M
Depends on: T-053
Paths: `crates/ss-audio/src/processing/`, `docs/design/voice.md`

## Goal
Give the `deepfilter` feature a real backend. T-053 shipped it as a
documented stub because no acceptable dependency existed.

## Scope
- Candidates, in order of preference recorded by T-053:
  - GTCRN, the best quality-per-CPU model reviewed, once a pure-Rust or
    ONNX streaming path is packaged on crates.io.
  - DeepFilterNet3 low-latency model, once a crates.io release exposes
    streaming inference (`deep_filter` 0.5.x with `tract` is git-only today,
    and `deny.toml` rejects git sources) and a model fits the 10 MB budget
    (the `_ll` ONNX bundle is 36 MB; the 8 MB standard model adds two frames
    of look-ahead, which the design rules out).
- Implement as a `Processor` stage in the existing chain: 10 ms frames,
  48 kHz, zero allocation per frame after setup, latency and CPU reported
  through `Monitor`.
- Model files bundled or fetched at build time; document the size and the
  licence; `cargo deny` must stay green.
- Same fixture tests as T-053: SNR gain ≥ 10 dB with ≤ 1 dB speech loss, and
  the bench budget of 1.5 ms per 10 ms frame.
- Update `docs/design/voice.md` processing table and the `ss-audio` README.

## Out of scope
Vendoring a model runtime into the repository; GPU backends.

## Blocked on
A crates.io release that provides streaming inference without git
dependencies and a model under 10 MB. Re-check quarterly; unblock when either
candidate qualifies, or decide to drop the feature and remove the stub.

## Acceptance criteria
- `cargo test -p ss-audio --features deepfilter` passes the fixture tests
  above; the stage is strictly better than RNNoise on the T-053 fixture.
- Listening comparison against RNNoise on real speech recorded in the notes.

## Notes from implementation
