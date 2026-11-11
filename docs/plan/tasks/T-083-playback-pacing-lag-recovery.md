# T-083 — Bounded playback latency: ring-level pacing and jitter lag recovery

Status: awaiting-acceptance
Phase: 3
Size: M
Depends on: T-015, T-041, T-082
Paths: `crates/ss-client-core/src/voice/worker.rs`, `crates/ss-audio/src/jitter.rs`, `crates/ss-audio/src/device/stream.rs`, `crates/ss-audio/src/device/convert.rs`, `crates/ss-audio/tests/`

## Goal
Output latency returns to its target after a disturbance. Today it only
grows during a call.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
measured. T-082 touches the same worker file; run this after it.

## Scope
- `Worker::run` renders 10 ms every 10 ms of `Instant` time while the device
  pops on its own clock. Each underrun inserts silence and the catch-up
  renders raise the ring level for good; a device clock slower than the
  system clock adds its drift (100 ppm is 360 ms per hour). Nothing trims
  the ring. At the 250 ms cap `push_playback` fails and the frame is dropped
  with `let _ =` and no counter. Drive rendering from ring fill (hold about
  10–20 ms, render when below target), or drop/shrink a quantum above a
  high-water mark. Count overflow drops and expose them in diagnostics.
- Non-48 kHz outputs chain a second 250 ms ring behind the resampler; bound
  the combined queue.
- `EchoPath.playback_queue` is hard-coded to 10 ms; feed the AEC the real
  queue depth.
- Jitter buffer: after a worker stall of 30–200 ms each talking track's
  deadline trails `now` by the stall and never recovers until the burst
  ends, which is never for open-mic or music senders. Contraction compares
  `target` with `applied` only. In `pop_for_playout`, when the lag exceeds
  one frame, re-anchor or skip-decode during low-energy audio.
- Stall sources on the audio worker: settings Apply builds the chain, VAD
  and devices there; device reopen runs there; dropping `DeviceManager`
  joins a thread that sleeps up to 500 ms. Move construction and teardown
  off the worker thread.
- The worker thread runs at default priority. Raise it with the same
  mechanism the device threads use and record the result per OS.

## Out of scope
Time-stretching (WSOLA) stays in the backlog. Device reopen policy (T-084).

## Acceptance criteria
- Simulation with a playback clock 200 ppm slow for a simulated hour: ring
  level stays within its target band.
- Simulation with a 150 ms worker stall during a continuous sender: output
  delay returns to within one frame of target in under two seconds.
- Overflow and trim counters appear in the diagnostics snapshot.
- `tests/sim.rs` asserts `late == 0` and the delay bound; today it asserts
  only `underruns == 0`, which cannot fail in a perfectly ticked simulation.
- `docs/design/voice.md` states the playback queue target and policy.

## Notes from implementation

### Findings, confirmed by tests written first
- Ring drift. A simulation of the real worker loop and a device that takes
  512-frame periods: with the old push, a device 200 ppm slow filled the ring
  to 240 ms of its 250 ms within the hour; 200 ppm fast gave 1078 underruns.
- Replayed stalls. `Worker::run` replays up to 30 ms after a stall, but the
  device has already played silence for it. Three 25 ms stalls raised the
  lowest queue from 0 to 21 ms, for good.
- Overflow. `let _ = push_playback(…)`: a full ring dropped quanta with no
  counter.
- Jitter lag. A continuous 20 ms sender and an 80, 150 or 190 ms worker stall:
  output delay stayed 70, 140 or 180 ms above the applied delay until the end
  of the run. A 30 ms stall was replayed by the worker and did recover.
- Not in the review: the capture `Framer`, `push_unframed` and the
  resampler's `push_all` stopped mid-frame when a ring filled. Half a stereo
  frame left in the ring swaps left and right from then on, and every later
  10 ms capture frame is shifted. A unit test left 3 samples in a ring.
- Resampled output (read, not run): the resampler drained the 48 kHz ring
  eagerly and, when the device-rate ring was full, dropped the rest of its
  batch, counted only on the playback stream's `overruns`, which nothing
  reads. The queue was bounded near 250 ms, but by silent, possibly
  mid-frame drops.
- `EchoPath` used a fixed 10 ms playback queue and a zero capture queue.
- Linux priority: `audio_thread_priority` without its D-Bus feature (how
  `cpal`'s `realtime` feature uses it) is a no-op that returns `Ok`. So
  `cpal`'s own Linux threads were never promoted either.

### What changed
- `voice/worker.rs`: `Pacer` sits between `render_once` and the ring. It
  watches the lowest queue ahead of a new quantum over 500 ms (what the
  device leaves after its callback), plus the resampler's queue. Band
  2.5–20 ms: above it, it drops quanta down to about 10 ms; below it, or when
  the device ran dry and the queue is still empty a quantum later, it inserts
  one silent quantum. It waits up to 3 s for a quantum at or below −60 dBFS,
  then cuts with 2.5 ms fades. Overflow drops are counted. The render tap
  (the AEC reference) gets exactly what reached the ring.
- `EchoPath` gets the measured playback queue and the capture ring's backlog.
- Counters: `WorkerStats.playback` (`PacingStats`: queue, overflow, trimmed,
  padded, forced) and `WorkerStats.realtime`. They surface in
  `VoiceSession::diagnostics()` as `playback.{queueMs,overflow,trimmed,padded,forced}`
  and `workerRealtime`, which the desktop diagnostics snapshot already
  returns under `details`. Per speaker, `caughtUp` (jitter). No desktop or
  `VoiceStats` changes.
- `jitter.rs`: `pop_for_playout` checks how far the release is behind its
  deadline. At a frame or more, it skips one queued frame per release while
  decoded audio is silent. After 500 ms behind, it skips the whole lag at
  once, also across missing slots, and returns the last skipped packet as
  `discarded` so the decoder keeps its history. It never skips across
  `TALK_END`/`TALK_START`. New `Stats::caught_up`. Expansion and contraction
  wait while it catches up.
- Stall sources: a `Setup` thread (normal priority) in the worker runs
  settings Apply (chain, VAD), device reopen after device events, backend
  switches, and drops old chains, streams and `DeviceManager`s. The worker
  keeps rendering, defers further Apply/backend updates until the job is in,
  and replies when it is. The manager is `Arc<Mutex<_>>`, and the worker only
  `try_lock`s it. Capture or playback alone can reopen while the other keeps
  running.
- Priority: `ss_audio::device::promote_audio_thread` wraps
  `audio_thread_priority` (new dependency, `default-features = false`,
  already in the lockfile through `cpal` on Linux). The voice worker calls it
  after setup, and the resampler threads call it at start.
- `stream.rs`: the resampled playback path splits the 250 ms budget: 50 ms at
  48 kHz, 200 ms at the device rate. The resampler pulls only whole frames
  that fit and stops reading the 48 kHz ring while the device ring is full.
  So overflow happens at, and is counted by, the worker.
  `PlaybackStream::downstream_queue` reports the device-rate ring (published
  by the callback after each pull) plus the resampler's held output.
- `convert.rs`: whole-frame pushes everywhere (`push_whole`), and
  `pull_playback` publishes what it left in the ring.

### Tests
- `voice/worker.rs`: `a_slow_playback_clock_keeps_the_queue_in_band_for_an_hour`
  (−200 ppm, one simulated hour, 256-, 512- and 1024-frame periods,
  continuous tone): no underruns or overflow after warm-up, 71 trims (720 ms),
  queue at most 20 ms plus one device period plus one quantum.
  `a_fast_playback_clock_is_padded_before_it_underruns` (+200 ppm, an hour):
  0 underruns, 73 pads. `speech_pauses_take_every_adjustment_without_cutting_audio`:
  with a pause every 3 s, no forced cuts at ±200 ppm.
  `short_stalls_do_not_raise_the_queue_for_good` (three 25 ms or 150 ms
  stalls). `a_full_queue_counts_every_dropped_quantum`.
  `settings_apply_off_the_worker_in_order_and_reject_invalid_values`.
- `ss-audio/tests/sim.rs`: renders now wake up to 3 ms late. The 2 % loss
  case asserts `late == 0` and a mouth-to-ear bound of 80 ms (30 ms network,
  40 ms buffer, one quantum). `continuous_sender_returns_to_target_delay_after_a_worker_stall`:
  `Voice`, `Music` and `LowLatency`, stalls of 30, 80, 150 and 190 ms with a
  loud tone. Delay is back within one frame of the applied delay after
  500 ms, with `late == 0` and no underruns.
  `quiet_sender_catches_up_in_silence_without_waiting` (silent sender,
  recovered within 200 ms).
- `tests/playout.rs`: `lagging_playout_skips_the_backlog_once_and_keeps_decoder_history`.
- `convert.rs`: `a_full_capture_ring_drops_whole_frames_and_keeps_stereo_aligned`,
  `playback_callback_publishes_what_it_left_in_the_ring`. `stream.rs`:
  ring budget, and `this_platform_accepts_a_real_time_audio_thread`, which
  passes on macOS.
- `just check` passes. `cargo clippy -p ss-client-core --features macos-voiceio
  --all-targets -D warnings` is clean.

### Deviations
- The acceptance criterion says "within one frame of target". The tests
  compare with the applied delay. With a steady network the target drops to
  20 ms, while a voiced burst keeps the 40 ms it started with until silence
  (existing T-015 behaviour).
- Rendering stays on the `Instant` clock, and the ring absorbs device drift.
  The other option, rendering from ring fill, would release jitter frames in
  bursts that follow the device period. A frame due 2 ms later would come
  out as silence in the middle of speech.
- Apple VoiceProcessingIO holds an Objective-C block and is not `Send`
  (`macos_voiceio.rs`), so opening and closing it stays on the worker. With
  that backend, Apply still stalls the worker; chain and VAD are built on
  the setup thread either way.
- Linux reports normal priority. Real promotion there needs rtkit over D-Bus
  (`audio_thread_priority/with_dbus`), which the dependency policy would have
  to accept.

### Files outside the Paths
- `ss-client-core/src/voice/mod.rs` (T-086's file): the `diagnostics()`
  JSON gains `playback`, `workerRealtime` and a per-speaker `caughtUp`
  (two hunks). No other change.
- `ss-audio/src/device/mod.rs`: re-exports `promote_audio_thread`.
- `Cargo.toml`, `crates/ss-audio/Cargo.toml`, `Cargo.lock` (one line): the
  `audio_thread_priority` dependency.
- `docs/design/voice.md`: new "Playback queue" section, and lag recovery in
  "Jitter buffer".

### For hands-on testing
No hardware listening was possible here. Suggested checks:
1. A call of at least an hour with the stats panel or diagnostics open.
   `details.playback.queueMs` should stay at about 25 ms or less, and
   `overflow` should stay 0. `trimmed` or `padded` should grow at about one
   per minute or less on a built-in output. A USB or Bluetooth DAC drifts
   more.
2. A 44.1 kHz output device (many USB headsets): same, with `outputRateHz`
   at 44100. Listen for left/right staying correct with a stereo music
   sender.
3. While a friend plays music or talks on open mic, click Apply in audio
   settings several times, switch the noise suppressor, and unplug or replug
   a headset. Their audio should not jump behind and stay behind; any
   catch-up is one short cut, and `caughtUp` grows.
4. Listen for forced cuts (`forced` grows) on continuous music: a short dip
   with no click.
5. Echo cancellation on, with speakers: echo should converge at least as well
   as before. The delay hint now includes the real queue.
6. Windows: check the log for "voice worker keeps normal priority". On
   Windows MMCSS is expected to succeed. Linux will log it.
