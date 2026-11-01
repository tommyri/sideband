# T-070 — Server: stream-based video tracks, subscriptions, keyframe relay

Status: done
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
- Implemented on `task/T-070-video-track-transport`; awaiting review and merge.
- Added StartStream/StopStream, Ack.track_id, private SubscriberHint and server
  RequestKeyframe events using new protobuf tags only. Welcome advertises
  `video-streams`. Updated protocol §3/§5 with framing, limits, and semantics.
- One VIDEO/SCREEN publication per joined connection requires VIEW_CHANNEL and
  STREAM. Video/audio share the non-reused track allocator. Subscriptions require
  another connection in the same voice channel with VIEW_CHANNEL; each client
  may subscribe to 16 video tracks. Codec descriptions are bounded opaque data.
- Video state shares the voice membership lock, so leave/switch/disconnect and
  channel deletion cancel publications/subscriptions together with voice seats.
  Permission refresh removes revoked publications and viewers and resets their
  in-flight groups. Snapshots and join announcements include visible video tracks.
  StopStream remains idempotent cleanup even after permissions are lost.
- Relay reads only the stream prefix, then fans out shared byte chunks as they
  arrive. Each publisher and each per-track subscription retain two groups;
  oldest groups are cancelled when the next arrives. Each downstream group has
  eight 16 KiB chunks of queue capacity. Overflow resets that viewer's group,
  never waits on it, and leaves other viewers running. A downstream FIN remains
  cancellable until Quinn acknowledges the buffered data. Sender reset propagates
  to all downstream groups. No GOP cache or whole-group buffering is used.
- Header/idle timeout is 5 s, total group lifetime 30 s, opaque group cap 32 MiB,
  four incoming group tasks per connection. Per-track subscription budgets avoid
  one publisher evicting an unrelated track; total subscriptions are also bounded.
  `stream_stats()` and internal Prometheus metrics expose active tracks, bytes,
  resets, and rejected groups. Audio forwarding logic is unchanged.
- Quality hints aggregate viewer count and highest requested height/fps per
  dimension; any unspecified (zero) preference leaves that dimension uncapped.
  Limits are 4320 pixels/240 fps. Keyframe requests require a subscription and
  relay at most once per 500 ms per track. Hints go only to the publisher.
- `ss-net::media::stream` provides GroupWriter/GroupReader and opaque Frame.
  Frames are 1..=4 MiB; the first is a keyframe, reserved flags are rejected.
  Reads retain partial prefixes/payloads across cancellation; cancelled writes
  reject reuse and reset on drop. `ss-client-core::media` adds VideoTrackSender
  and a single connection-wide VideoTrackReceiver for demultiplexing track ids.
- Five authenticated loopback integration tests cover streaming before FIN,
  identical delivery to two healthy viewers while a third stalls, no delivery
  to an unsubscribed peer, private quality/keyframe relays and coalescing,
  ownership rejection, sender resets, old-group eviction, membership cleanup,
  duplicate publication rejection, snapshot inclusion, non-reused track ids,
  and STREAM permission revocation. A transport test covers cancelled partial
  frame reads and malformed/oversized frame lengths and flags.
- Validation: `just check` passed (258 tests passed, 1 manual load test
  ignored); `just check-deps` and `git diff --check` passed. Updated the old
  auth test to expect voice-membership validation for SubscribeTrack, which
  is now implemented rather than an unsupported command.
