# T-082 — Correct talk-burst boundaries for voice activation and gate close

Status: todo
Phase: 3
Size: S
Depends on: T-052, T-041
Paths: `crates/ss-client-core/src/voice/worker.rs`, `crates/ss-client-core/src/voice/graph.rs`, `crates/ss-audio/src/vad.rs`, `crates/ss-audio/tests/`

## Goal
Every burst ends with `TALK_END` and the next one starts with `TALK_START`
and a timestamp gap, in all three gate modes.

Source: 2026-11-09 review at `d861a0a`. Sender side traced by reading the
code; the receiver effect was traced by the reviewer and not reproduced.
Start with the failing graph test.

## Scope
- `capture_vad` pushes the gated quanta, then one zero quantum with
  `gate = false` on `Close`, and then stops calling `push_capture` while the
  gate stays closed. The graph frames 20 ms from 10 ms quanta, so when the
  closing quantum is the first half of a frame it stays in `pending`:
  `talking` remains true, no `TALK_END` is sent and the capture timestamp
  freezes. The next utterance then carries no `TALK_START` and no gap.
  Reported receiver effect: several PLC frames, the first ~140 ms of the
  next utterance dropped as late, and the burst anchored at the maximum
  jitter delay. Call `push_capture(.., false)` for every closed-gate quantum,
  as the push-to-talk path does.
- The `TALK_END` frame can carry up to 20 ms captured after mute or PTT
  release; only the VAD close path zeroes it. Zero the closing quantum on
  every gate path.
- `Vad::process` rejects a frame with any `|sample| > 1.0` and the worker
  drops it silently, without advancing the timestamp. Nothing upstream
  clamps (resampler overshoot, RNNoise or AEC output, float devices). Clamp
  before the VAD or treat over-range as active; keep rejecting non-finite
  samples.
- The sender's `snapshot.talking` and speaking attenuation follow the same
  flag; check they clear at the end of a burst.

## Out of scope
VAD tuning and thresholds. Playback pacing (T-083).

## Acceptance criteria
- Graph-level test: bursts of odd and even quantum counts, for 10 ms and
  20 ms profiles and for VAD, PTT and open mic with mute, each end with
  exactly one `TALK_END` and the following burst starts with `TALK_START`
  and an advanced timestamp.
- Worker-level test drives `TalkGate::VoiceActivation` (the integration
  tests use `OpenMic` only today).
- A frame containing samples above full scale is transmitted, not dropped.
- Receiver test: two bursts separated by silence lose no leading packets.

## Notes from implementation
