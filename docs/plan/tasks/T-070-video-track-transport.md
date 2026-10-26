# T-070 — Server: stream-based video tracks, subscriptions, keyframe relay

Status: todo
Phase: 5
Size: M
Depends on: T-025, T-004
Paths: `crates/ss-server/src/media/streams.rs`, `crates/ss-net/src/media/stream.rs`, `crates/ss-client-core/src/media/`

## Goal
The server can carry a screen-share track per `02-protocol.md` §5 without
knowing anything about video.

## Scope
- `StartStream{kind=SCREEN, codec}` / `StopStream` client messages (add to
  proto) under `STREAM` permission; `TrackAdded` with codec description;
  one video track per connection in v1.
- Sender → server: accept unidirectional streams, parse `StreamHeader`,
  validate track ownership, and copy bytes to subscribers' outgoing uni
  streams as they arrive (pipe, not store-and-forward); on sender reset,
  reset downstream.
- Subscriptions: `SubscribeTrack/UnsubscribeTrack`; per-subscriber send
  budget (e.g. 2 groups in flight); when exceeded, reset the oldest group's
  downstream stream (drop-oldest) and count it; `SetPreferredQuality` stored
  per subscriber and forwarded to the sender as `SubscriberHint{max_height,
  max_fps, viewer_count}` (add to proto) so the sender can adapt (T-074).
- `RequestKeyframe` relayed to the sender, coalesced to ≤ 1 per 500 ms.
- Metrics: streams active, bytes forwarded, resets.
- `ss-client-core` plumbing: open/accept uni streams, expose
  `VideoTrackSender`/`VideoTrackReceiver` byte-level APIs (no codecs).

## Out of scope
Capture/encode/decode (T-071/T-072), audio of the shared app (backlog).

## Acceptance criteria
- Integration test with synthetic "frames": one sender, three subscribers,
  one deliberately slow → slow one gets resets, others unaffected; keyframe
  request reaches sender; unsubscribed client receives nothing.

## Notes from implementation
