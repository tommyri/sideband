# T-074 — Adaptive video bitrate and quality selection

Status: todo
Phase: 5
Size: M
Depends on: T-070, T-071, T-072
Paths: `crates/ss-video/src/abr.rs`, `crates/ss-server/src/media/streams.rs`

## Goal
Streams stay smooth when someone's connection is weaker, without the
streamer touching anything.

## Scope
- Sender-side controller fed by: own `quinn` stats (RTT, loss, cwnd), server
  `SubscriberHint` (lowest preferred quality among viewers, viewer count,
  reset counts per subscriber relayed as `SubscriberStats`), encoder queue
  depth. Output: target bitrate (continuous within preset) and preset
  switches (with a keyframe) using additive-increase/multiplicative-decrease
  with hysteresis.
- Server reports per-subscriber resets/queue to the sender every 1 s
  (`SubscriberStats`, add to proto).
- Viewer-side: if a viewer is the bottleneck, it may request a lower
  `SetPreferredQuality` automatically (opt-in "auto quality" default on).
- Later option noted: simulcast (two encodes) if one viewer is much slower;
  not in scope.

## Out of scope
Simulcast/SVC.

## Acceptance criteria
- Impairment test (`--impair` on a viewer): bitrate drops within 3 s, no
  freeze > 1 s, recovers within 30 s after impairment ends.
- Streamer on a stable link with one slow viewer keeps full quality for the
  others (viewer-side downgrade only).

## Notes from implementation
