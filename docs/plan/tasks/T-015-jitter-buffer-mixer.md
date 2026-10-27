# T-015 — `ss-audio`: adaptive jitter buffer, per-track decode, mixer

Status: done
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


Implemented on `task/T-015-jitter-buffer-mixer`; merged locally to `main`.

- `jitter::PacketHeader` is audio timing metadata, mapped from validated media
  headers by the future client-core integration; `ss-audio` still has no
  dependency on `ss-net` or `ss-proto`. Each track preallocates 64 packet slots.
  Sequence and capture timestamps wrap at their wire widths.
- Delay starts at 40 ms and uses the 95th percentile of up to 200 forward
  inter-arrival deviations from the last two seconds, plus a 20 ms margin,
  rounded up to a codec frame and clamped to 20–200 ms. Late forward arrivals
  inform the estimate even though their audio cannot replace committed slots.
  Stale estimates reset after a long idle pause.
- A burst boundary reanchors playout. Lost boundary flags can be inferred from
  a whole-frame capture-clock gap beyond the packet sequence distance. A newer
  burst can reset speculative PLC slots after a lost TALK_END; old/reordered
  start flags cannot replay audio. During a burst, only decoded silence below
  −60 dBFS permits insertion/contraction. Skipped silence is still decoded to
  preserve codec state. Time stretching and continuous clock-drift correction
  remain deferred.
- `TrackReceiver` emits fixed 10 ms stereo quanta, converting mono as needed,
  with per-track gain and a 200 ms speaking timeout. The speaking indicator is
  presentation state, not a mixing gate: callers must still drain queued tails.
  Mixer accepts per-user gains and UI frames, applies master gain, then a
  linked stereo soft knee at 0.8. Linear gains are validated within 0..=4.
- Recovery occurs before playback commitment: FEC uses an already-buffered next
  packet; otherwise PLC fills the slot. With DRED, buffered future redundancy
  can recover multiple slots into preallocated output storage. Reserved slots
  reject late originals. The scope's wording about replacing audio *after* PLC
  is interpreted as pre-release recovery; already-played audio cannot be fixed
  retrospectively. Counters distinguish explicit PLC from FEC attempts (libopus
  may internally use PLC when no FEC is encoded).
- Four deterministic 10-second simulations with 30 ms ± 10 ms network delay
  and 2% independent loss: zero underruns, zero RMS-gap quanta, 40 ms buffering,
  first output scheduled at 70 ms (30 ms network + 40 ms buffer). This measures
  scheduling, not end-to-end device/codec latency; T-017 measures that separately.
  The 5% three-packet-burst simulation with 10% reordering reports 50 missing
  playout slots (including 23 late arrivals), 26 PLC calls and 24 FEC attempts;
  zero RMS-gap runs. The audible-gap proxy is RMS below −60 dBFS, not a listening
  quality score.
- Tests cover wraparound, initial reordering, duplicate/late admission, bounded
  queue/window growth, idle/stall recovery, burst flag loss, silence-only delay
  changes, gains, stereo limiting, UI injection, and streaming allocations.
  Default and optional DRED paths measure zero Rust heap allocations after
  construction; the allocation counter does not instrument native C allocators.
- Device-ring wiring belongs to T-010/T-041; these APIs run on the playback
  worker and return frames for that integration. No networking or UI is added.
- Validation on macOS: `just check` passed (139 tests, one manual test ignored);
  optional-DRED tests passed (29); optional-feature clippy and `just check-deps`
  passed. Merged locally at the user’s request; nothing pushed.
