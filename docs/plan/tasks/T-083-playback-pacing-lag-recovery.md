# T-083 — Bounded playback latency: ring-level pacing and jitter lag recovery

Status: todo
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
