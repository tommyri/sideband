# T-074 — Adaptive video bitrate and quality selection

Status: todo
Phase: 5
Size: L
Depends on: T-070, T-071, T-072, T-100
Paths: `crates/ss-video/src/`, `crates/ss-server/src/media/streams.rs`, `crates/ss-proto/proto/socialspeak/v1/voice.proto`, `crates/ss-client-core/src/media/`, `apps/desktop/src-tauri/src/stream.rs`, `apps/desktop/src-tauri/src/viewer.rs`, `docs/protocol/`, `docs/self-hosting.md`

## Goal
Streams stay smooth when someone's connection is weaker, without the
streamer touching anything. A slow viewer drops to a lower picture; the
other viewers keep full quality.

Rewritten 2026-11-16 after T-100 kept the QUIC backend. One encoded
rendition cannot serve a slow viewer without lowering quality for everyone,
and the server never transcodes (D4). So a sharer publishes a second, lower
rendition, and the server picks one per subscriber at a group boundary.

## Scope
- **Second rendition.** The sharer encodes the captured frames twice: the
  full rendition (the preset, for example 1080p30) and a low one (360p15,
  about 300 kb/s, the layer T-100 measured), scaled from the same frame.
  Each rendition is its own run of GOP streams, starting at a keyframe.
  The low encoder runs only while the server reports a subscriber on it;
  starting it costs a keyframe and counts toward the 3 s target below.
- **Protocol.** Only add fields, never reuse tags. Mark each GOP stream's
  rendition in its header, and announce both renditions' dimensions and
  frame rate with the track. Record the wire change in `docs/protocol/`.
- **Server selection.** Per subscriber, the relay forwards one rendition.
  It switches only at a group boundary, to a group that starts with a
  keyframe:
  - down, when that subscriber's queue goes over budget or its group is
    reset;
  - up, after a quiet period with hysteresis, so the subscriber does not
    flap;
  - capped by that viewer's `SetPreferredQuality`.

  The per-subscriber queue and drop-oldest rule (screen-sharing rule 3)
  stay as they are.
- **Sender-wide bitrate.** A controller in `ss-video` adjusts the full
  rendition's bitrate within its preset. Its inputs are the sender's own
  `quinn` stats (RTT, loss, congestion window), encoder queue depth, and a
  new per-second `SubscriberStats` from the server (resets and queue depth
  per rendition). It uses additive increase, multiplicative decrease, with
  hysteresis. It lowers the full rendition only when the uplink itself is
  short, not for one slow viewer.
  - A bitrate or frame-rate change never forces a keyframe. Discord found
    that its AMD encoders did this.
  - The rate is held as an average over several frames, not as a budget
    for each frame, which undershoots.
- **Fallback.** If the second encoder cannot open (no second hardware
  session), the sharer publishes one rendition, and a slow viewer gets
  sender-wide adaptation only. Log it once and show it in diagnostics.
- **Viewer.** Draw whichever rendition arrives, scaled to the window. A
  switch must not show a gap or a stale frame. The desktop's per-stream
  rule stays: a failed GOP stream is skipped, and the next one is still
  accepted.

## Out of scope
SVC. More than two renditions. Server-side transcoding. Screen-share audio.

## Acceptance criteria
- **Impairment run** (T-100's setup): two viewers, one shaped to 600 kb/s
  with 2 % loss and +40 ms for 30 s, at 1080p30.
  - The impaired viewer shows the low rendition within 3 s.
  - After that switch, it never freezes for more than 1 s.
  - It is back on the full rendition within 30 s after the impairment ends.
  - The healthy viewer stays on the full rendition at 30 fps throughout,
    with no skipped group.
  - Record how much of the low rendition's bitrate goes on keyframes with
    the 2 s GOP. Keyframes cost 6 to 10 times a delta frame. If the share
    is large, try a longer GOP on the low rendition, with a keyframe sent
    whenever a viewer switches to it, and keep whichever passes.
- **Frame rate:** full-motion 1080p30 keeps 30 fps on the full rendition
  while the low one is encoding. LiveKit's simulcast lost two thirds of its
  frame rate here.
- **Sender CPU:** with both renditions on, the sharer stays inside T-071's
  encode budget (under 25 % of a core on macOS). Record the figure.
- **Fallback test:** with the second encoder refused, the call goes on
  with one rendition and sender-wide adaptation.
- **Server upload per viewer:** measure what the server sends per viewer
  for each rendition. Write it in `docs/self-hosting.md` so an operator can
  size the server's uplink, and say there that nothing caps viewers per
  stream (Discord caps one stream at 50 viewers). Choosing a cap is the
  maintainer's call and is not part of this task.
- **Relay tests** in `crates/ss-server/tests/video_streams.rs`:
  - a subscriber that stops reading for many groups is reset each time,
    while a healthy one keeps receiving every new group;
  - a switch happens only at a keyframe group.

  This also settles the relay stall T-100 could not rule out.

## Notes for the implementer
T-100's harness in `experiments/livekit-spike/` measures exactly these
numbers. It has a user-space UDP impairment proxy (`src/impair.rs`), a
barcode latency stamp (`src/stamp.rs`), and its `base-*` runs (`run.sh`).
Its results are in
[the research note](../notes/screenshare-livekit-research.md#results-2026-11-16-macos);
on today's backend, the impaired viewer froze for 27 of the 30 s.

How Discord and TeamSpeak share screens, and where the keyframe, rate and
viewer-cap points above come from:
[Discord and TeamSpeak research](../notes/screenshare-discord-teamspeak-research.md).

## Notes from implementation
