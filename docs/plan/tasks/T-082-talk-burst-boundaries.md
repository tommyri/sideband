# T-082 — Correct talk-burst boundaries for voice activation and gate close

Status: in-progress
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

### Finding confirmed
The failing tests reproduced the review on the unchanged code (`1e0b0ba`):
- Worker VAD, `Voice` profile, 50 ms hangover: the first burst was 2 pre-roll
  + 1 speech + 5 hangover quanta, plus the synthetic zero quantum, which is 9
  quanta. Half a frame stayed in `pending`, so no `TALK_END` was sent. The
  next utterance went out with flags `0` and a timestamp contiguous with the
  previous burst, across 200 ms of silence. The capture clock also read 0 at
  the first speech, which came after 200 ms of leading silence: it never
  advanced while the gate was closed.
- Receiver, two VAD bursts separated by silence: 7 leading packets `late`
  and 7 `lost` (PLC) on `Voice`, out of 34 sent.
- A frame with a sample above full scale was dropped by `Vad::process`, and
  the worker returned without pushing anything: no packets at all in that
  test.
- Graph only: a one-quantum burst that started on a 20 ms frame boundary was
  dropped (`[open, closed]` finished with `gate = false` and `!talking`).
- PTT and open-mic closing quanta carried the captured audio into the
  `TALK_END` frame (up to 20 ms when the burst had an even quantum count).

### What changed
- `VoiceGraph::push_capture` (graph.rs) is called once per captured quantum
  whatever the gate. A closed quantum is never transmitted. Inside a burst it
  is appended as zeros, so the `TALK_END` frame holds silence (and flushes the
  encoder) on every gate path. Between bursts it only advances the timestamp,
  with `pending` left empty, so a burst can start on any quantum and the next
  `TALK_START` carries the real capture position. A frame that holds gated
  audio is always sent; `[open, closed]` from idle is one packet with
  `TALK_START | TALK_END`.
- New `VoiceGraph::retract_closed`. Voice activation replays up to two quanta
  of pre-roll that the graph has already counted as closed. Without retraction,
  advancing the clock in silence would push the capture clock (and the video
  anchor in `media/sender.rs`) 20 ms ahead per burst. Retraction only takes
  back closed quanta that have not been sent, so a new burst never overlaps
  the previous `TALK_END` frame. If the gate reopens while a closing quantum
  is still in `pending`, the replayed audio replaces it and the burst simply
  continues.
- `capture_vad` (worker.rs) pushes every quantum. Gated output goes in as
  open. Closed, muted, calibrating and rejected (non-finite) quanta go in as
  closed. The synthetic zero quantum on `Close` is removed: the next real
  closed quantum ends the burst, 10–20 ms after the last gated audio.
- `Vad::process` (vad.rs) rejects only non-finite samples. Over-range finite
  samples are measured as they are and returned bit-identical.
- `snapshot.talking` and speaking attenuation read the same flag, which
  `TALK_END` now clears. Every gate test asserts it is false after the script.

### Deviations
- Over-range: the task offered "clamp before the VAD or treat over-range as
  active". I did neither. The detector measures unclamped samples, which can
  only raise the level compared with clamping. Transmitted PCM is unchanged:
  the Opus float encoder accepts finite values above 1.0, and the receiver's
  limiter handles them. A lone over-range click in a quiet frame does not
  force the gate open; a test covers this.
- `retract_closed` is new. The task did not ask for it. It is needed so that
  "a closed quantum advances the clock" does not drift the capture clock
  forward by the pre-roll on every VAD burst.
- With a 20 ms frame, two closed quanta are needed to end a burst. A single
  closed quantum between two open ones continues the burst (with 10 ms of
  silence) and sends no `TALK_END`/`TALK_START` pair. This is inherent to the
  framing, and the tests use gaps of 2 or more.

### Tests
- graph.rs: `every_burst_ends_with_talk_end_and_the_next_starts_after_a_gap`
  covers bursts of 1–5 quanta, gaps of 2, 3 and 7, and the `Voice`, `Music`
  and `LowLatency` profiles.
  `closing_quantum_is_zeroed_even_when_the_microphone_still_hears_speech` and
  `pre_roll_retraction_rewinds_only_unsent_closed_quanta` are also new. The
  shared `assert_bursts` checks one `TALK_START` per burst at the exact
  capture position of its first sample, contiguous seq and timestamps,
  exactly one `TALK_END` on the last packet, a `TALK_END` frame that holds
  the first closed quantum, and no overlap with the previous burst.
- worker.rs (new module): `every_gate_ends_each_burst_once_and_restarts_after_a_capture_gap`
  builds a real `Runtime` with `AudioSource::Silent` and calls `capture_once`
  for `PushToTalk`, `OpenMic` with mute, and `VoiceActivation`. It runs all
  three profiles, with bursts of 1–6 quanta, so the gated count is odd and
  even. `voice_activation_transmits_samples_above_full_scale` and
  `receiver_loses_no_leading_packets_across_voice_activation_bursts` are also
  new. The receiver test sends two or more VAD bursts through a receiving
  `VoiceGraph` in real time and asserts `accepted == sent`, `late == 0` and
  `lost == 0` for the 20 ms and 10 ms profiles.
- ss-audio/tests/vad.rs: `samples_beyond_full_scale_are_measured_and_returned_unchanged`
  is new. The malformed-frame test still rejects NaN and ±infinity.
- `just check` passes. `cargo clippy -p ss-audio --features rnnoise --all-targets`
  is clean.

### For hands-on testing
No hardware listening test was possible on this machine. Suggested checks, on
two clients in one channel:
1. VAD on both. Say short words with pauses longer than the hangover. Check
   that the listener hears the onset of each word, with no warble or PLC tail
   between words. Check that the speaking indicator clears between words, and
   that the talker's attenuation of others releases.
2. PTT: release mid-word. The tail should stop cleanly at release (plus the
   release delay), with no extra 10–20 ms of speech.
3. Open mic: mute mid-word. Same expectation.
4. With screen share on and VAD, after a few minutes of intermittent speech,
   lip sync should not drift.

### Follow-ups
- `crates/ss-cli/src/graph.rs` has its own copy of the graph. It pushes every
  quantum, so the stuck-burst bug does not apply. It still sends captured
  audio in the closing quantum. Left alone because it is a measurement tool
  and outside this task's paths.

