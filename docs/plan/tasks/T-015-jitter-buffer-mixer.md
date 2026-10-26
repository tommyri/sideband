# T-015 — `ss-audio`: adaptive jitter buffer, per-track decode, mixer

Status: todo
Phase: 1
Size: M
Depends on: T-014
Paths: `crates/ss-audio/src/jitter.rs`, `crates/ss-audio/src/mixer.rs`, `crates/ss-audio/src/receive.rs`

## Goal
Turn jittery, lossy, reordered datagrams from N speakers into one smooth
48 kHz output with the lowest delay that avoids underruns.

## Scope
- `JitterBuffer` (per track): sorted by `seq`; `push(header, packet)`;
  `pop_for_playout(now) -> Packet | Loss | Wait`. Target delay adapts to a
  running jitter estimate (e.g. 95th percentile of inter-arrival deviation over
  the last 2 s) clamped to [20 ms, 200 ms]; initial 40 ms. Expansion /
  contraction only at talk-burst boundaries (TALK_START/TALK_END flags) or in
  decoded silence, by inserting PLC frames or dropping a frame; no
  time-stretching in v1 (documented follow-up).
- Late packets: if a packet arrives after its slot was PLC'd but FEC in the
  next packet can recover it, use FEC; otherwise discard. Counters for
  late/lost/duplicate/PLC.
- `TrackReceiver`: JitterBuffer + `Decoder` + per-track gain + speaking state
  (flags + 200 ms timeout) → emits 10 ms f32 frames.
- `Mixer`: sums active tracks into stereo f32 with per-user gain, a master
  gain, soft-knee limiter, and an injection input for UI sounds. Runs in the
  playback worker, never in the device callback directly (feeds the rtrb ring).
- Simulation harness `tests/sim.rs`: synthetic network with configurable
  delay distribution, loss %, reorder %, burst loss; asserts on gaps,
  underruns, and achieved delay.

## Out of scope
Networking, UI.

## Acceptance criteria
- Sim: 30 ms ± 10 ms jitter, 2 % loss → zero underruns, achieved delay
  ≤ 70 ms, no gap > 20 ms.
- Sim: 5 % loss with 3-packet bursts → audible-gap count reported; FEC/PLC
  engaged (counters > 0).
- Mixer: three −12 dBFS sines don't clip; limiter engages above 0 dBFS.
- No allocation after construction (`assert_no_alloc` in tests).

## Notes / references
- NetEQ design (adaptive target delay, PLC, expand/accelerate) as reference;
  Luby's ARC (2025) for receiver-side release scheduling ideas.

## Notes from implementation
