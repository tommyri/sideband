# T-074 — Adaptive video bitrate and quality selection

Status: awaiting-acceptance
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

2026-11-16, on `task/T-074-adaptive-bitrate` (from `main` at `0c362a0`).
Builds on T-071 and T-072, both `awaiting-acceptance` (merged, hands-on
checks open), as the plan allows.

### What was built

- **Second rendition** (`ss-video`). `Encoders` is the encode stage that
  `Publisher` now wraps; it also takes frames from a caller, which is how the
  harness drives it. It opens the full encoder, then probes a second
  hardware-only session for the low rendition: 640×360, 15 fps, 300 kb/s,
  scaled (GPU resize on macOS) from the same captured frame and decimated by
  capture time. The low encoder is fed only while the server reports a viewer
  on it; resuming forces a keyframe. Its GOP is 10 s (`LOW_GOP`), with a
  keyframe whenever a viewer switches to it (see keyframe share below).
- **Protocol** (`voice.proto`, new tags only). `Rendition{index, width,
  height, fps, bitrate, codec}` as `StartStream.renditions` (3),
  `Track.renditions` and `TrackAdded.renditions` (7);
  `SubscribeTrack.renditions` (2), the viewer's opt-in;
  `RequestKeyframe.rendition` (2); server event `SubscriberStats` (envelope
  tag 33) with per-rendition `RenditionStats`. Bit 0 of a video stream
  header's flags (`ss_net::media::LOW_RENDITION`) marks rendition 1; older
  parsers already accept it. Each rendition numbers its own groups.
  Recorded in `02-protocol.md` §3, §5 and §7 and the regenerated
  `docs/protocol/messages.md`.
- **Server selection** (`media/streams/adapt.rs`, `relay.rs`). One rendition
  per opted-in subscriber, switched only when a new group of the target
  rendition begins. Down: when the connection's link falls behind or one of
  its groups is reset because it fell behind; its full groups are cancelled
  and a low keyframe is requested (or, for the first low viewer, the
  publisher starts its low encoder on the immediate `SubscriberStats`). Up:
  after a quiet period of 4 s, doubling per failed probe up to 16 s, the next
  full group is forwarded at a lower QUIC stream priority while the low one
  keeps flowing; it is cancelled if the link falls behind, otherwise the
  viewer moves up at the next full group at least 1 s later. Capped by
  `SetPreferredQuality`. Group budgets are now per rendition, and a finished
  incoming group is no longer cancelled by the publisher's budget, so a
  healthy viewer's tail is not reset at a boundary. The drop-oldest rule is
  unchanged.
- **Sender-wide bitrate** (`ss_video::rate::RateController`, fed by
  `ScreenShareSession` every 500 ms). AIMD within the preset (one eighth of
  its bitrate up to all of it): ×0.75 after two bad ticks, +5 % of the preset
  after three good ones, no increase for 5 s after a decrease. Inputs: the
  sender's own Quinn stats (video written against bytes sent and not lost,
  round-trip rise over a 20 s minimum) and `SubscriberStats` (congested
  full-rendition viewers). With a low rendition, viewer congestion counts
  only when every full-rendition viewer (at least two) is congested; without
  one, any congested viewer counts. Applied with `set_bitrate` (VideoToolbox
  `AverageBitRate`), which never forces a keyframe and is an average, not a
  per-frame budget.
- **Fallback.** If the second session does not open, one rendition is
  announced; it is logged once (`warn`) and the desktop share status says
  "one rendition (no second hardware encoder)" (`LowState` in
  `EncoderStats`). `LowRendition::Off` refuses it on purpose (fallback test).
- **Viewer** (`ss_client_core::media::Switcher`, desktop `viewer.rs`). The
  demux keeps the newest pending group per track and rendition. The switcher
  plays one lane and buffers the other from its keyframe; it moves up once
  the better lane has caught up with the picture on screen, and down when
  the lane on screen fails or stalls. A lane it leaves keeps its decoder
  state, so a failed probe falls back without a keyframe. One decoder per
  rendition; the last picture stays until the next one decodes. A failed
  GOP stream is skipped and the next one is still accepted.

### Measurements

macOS 27.0.1, M4 Pro MacBook Pro (T-100's machine), Rust 1.99.1, loopback
server. Harness: `experiments/livekit-spike` roles `t074-pub` / `t074-view`
(they drive the product code above), `run.sh` scenarios `t074-*`,
`analyze_t074.py`. Content `pattern` (T-100's screen-like pattern) unless
noted; 1080p30 preset, 4 Mb/s. Impairment: `viewer2` through `impair`, 20 s
clean, 30 s at 600 kb/s with 2 % loss and +40 ms, then clean; `viewer1`
direct. Five impairment runs: three with the final 10 s low GOP, two with a
2 s low GOP aligned to the full rendition.

| Criterion | Result |
|---|---|
| Impaired viewer shows the low rendition within 3 s | **Pass.** First low frame 1.03–1.16 s after the impairment began (server moved it down after 0.52–0.63 s). The freeze before that, on the stalled full group, was 1.05–1.16 s |
| After the switch, no freeze over 1 s | **Pass.** Longest gap between presented frames 305, 352 and 366 ms (10 s low GOP); 659 and 670 ms (2 s GOP). No gap over 1 s. 15 fps median, about 295 kb/s |
| Back on full within 30 s after the impairment ends | **Pass.** Viewer clock (first presented full frame after which no low frame followed), per run: 0.17, 14.18 and 0.18 s (10 s GOP); 12.19 and 12.17 s (2 s GOP). Server clock (the probe confirmed at the next full group): 2.1, 16.2 and 2.2 s; 14.2 and 14.2 s. The viewer is earlier because it moves to a probe as soon as the probe catches up |
| Healthy viewer on full at 30 fps, no skipped group | **Pass.** Per-second minimum 29.4–29.7 fps, median 30; all 50 full groups received in order with no group-id gap and no low frame; the switcher reported no lost group. Barcode frame skips on the healthy viewer (23–65 per run) were never more than the publisher's own skips at the source in the same run (27–74), so none was lost in transport |
| Low rendition keyframe share | 2 s GOP aligned with the full one: 21–24 % of the low rendition's bytes on `pattern`, 59 % on a mostly static live desktop. 10 s GOP with a keyframe on each switch: 7.5–7.6 % on `pattern`, 24 % on the desktop (where the low rendition is only 55 kb/s). Both pass; kept 10 s |
| Full-motion 1080p30 keeps 30 fps with the low one encoding | **Pass.** `pattern-hard`, one viewer on each rendition: sender 30 full frames every second, viewer 29.1–29.5 fps minimum, 30 median; full rendition 4.0 Mb/s at the viewer, low 0.30 Mb/s. Two VideoToolbox sessions also encoded full-motion noise at 79.5 fps unpaced (ignored test `videotoolbox_two_sessions_hold_full_motion_1080p30`, `--release`) |
| Sender CPU with both renditions on, under 25 % of a core | **Pass: 3.6–4.1 %** median on live ScreenCaptureKit capture of the display (2.7 % with one rendition). `pattern` runs: 10.4–12.2 %, of which about 10 points is generating the pattern (T-100). Combined encode wall time per captured frame, both renditions: 16.1–17.5 ms median on capture (11.7 ms for one rendition), so the two together exceed T-071's 15 ms per-frame encode target; the frame rate still held (29.3 fps, the capture rate T-100 also saw) |
| Fallback: second encoder refused | **Pass**, the call goes on: one rendition announced, the healthy viewer kept 29.7 fps with no skipped group, and the rate controller lowered the full rendition to its 0.5 Mb/s floor 9 s after the impairment began, recovering to 4 Mb/s 28 s after the impairment ended. The impaired viewer, with sender-wide adaptation only, got 21.5 fps on average but 9.4 s of the 30 s in gaps over 1 s (worst 3.9 s), latency p50 0.54 s. T-100's baseline froze 27 of 30 s |
| Server upload per viewer and rendition | Full 1080p30: 0.9 Mb/s (desktop), 1.8 Mb/s (`pattern`), 4.0 Mb/s (`pattern-hard`). Low: 0.15, 0.30, 0.30 Mb/s. Relay CPU 1.0–1.5 % of a core. In `docs/self-hosting.md` with "nothing caps viewers per stream" |
| Relay tests | **Pass.** `stalled_subscriber_is_reset_every_group_while_healthy_one_gets_each_group` (12 groups of 1.5 MiB, every stalled-viewer group reset, the healthy one gets all 12 complete and in order) and `rendition_switches_happen_only_at_a_new_group` (cap, legacy viewer, probe up). This settles the relay stall T-100 could not rule out: a viewer that stops reading cannot stall another viewer |

### Deviations

- **Congestion signal.** The spec names queue over budget and group resets.
  Quinn buffers more than a GOP per stream, so the relay's chunk queue rarely
  overflowed: the first T-100-style run moved the impaired viewer down only
  2.6 s after the impairment began. The server therefore also compares, per
  connection over 0.5 s, the video it wrote with the bytes Quinn sent and did
  not declare lost, and watches the round-trip time against its ten-second
  minimum (+60 ms); either for 300 ms is congestion. Resets still count. The
  RTT rule re-baselines after 10 s, so a lasting route change does not keep a
  viewer low; the test's +40 ms delay is why the first probe came 16 s into
  the impairment.
- **Low encoder lifetime.** The second session is opened once at start (that
  is the fallback probe) and kept; "runs only while a viewer is on it" means
  it is fed frames only then. Opening a session mid-share would stall the
  capture thread and could fail after the track was announced.
- **An unexplained move of the healthy viewer.** In the first signal check
  (T-100's baseline publisher announcing a low rendition it never sent), the
  healthy viewer was moved down once, 39 s in, right after the impaired
  viewer's keyframe requests forced two full keyframes. Which trigger fired
  is unconfirmed: the switch log was added afterwards. Raising the per-viewer
  chunk queue from 8 to 32 chunks (512 KiB; chunks are shared `Bytes`, so
  only a viewer that falls behind holds them) was a guess at an overflow on
  that burst. It was not seen again in 19 later runs (5 signal checks and 14 audited
  `t074-*` runs). Every other down-switch outside an impairment came 1.4–3.4 s
  after the first viewer process had exited, when its connection closes:
  an exit artifact of the harness.
- **Rate controller inputs.** The spec names Quinn's congestion window and
  the encoder queue depth. The controller uses video bytes written against
  bytes Quinn sent and did not lose, and the round-trip rise, instead: on
  this transport the queue builds in Quinn's send buffer, which neither the
  congestion window nor the encoder sees, and a blocked write already stalls
  the capture loop.
- An unpaced in-process burst (1.5 MiB groups written as fast as possible on
  one test runtime) did reset the healthy viewer in the relay test; the test
  paces at about 128 Mb/s, far above any encoder.
- A per-rendition codec description is limited to 4 KiB. Video tracks were
  already missing from the `Welcome` size bound; recorded in `BACKLOG.md`.
- The probe up uses QUIC stream priority, so it only takes spare capacity in
  Quinn's send buffer; it still fills the bottleneck queue briefly, which is
  where the 305–670 ms gaps come from.

### Not exercised here

- `ScreenShareSession`'s rate and server-report glue and the desktop
  `viewer.rs` loop compile, pass clippy and have unit tests for their pure
  parts (`start_stream`, `viewer_report`, the switcher), but no test or
  measurement ran them: the harness runs its own equivalents around the same
  product components (`Encoders`, `RateController`, `RenditionWriter`,
  `Switcher`). `viewer_smoke` covers only `viewer_gpu`.
- The round-trip rise trigger was only exercised on loopback and through
  `impair`. A healthy viewer on Wi-Fi, whose round trip jitters, is part of
  the VPS acceptance row.
- Windows and Linux: whether a second hardware session opens, and the
  frame rate with both on.

### Left for hands-on acceptance

Rows in `ACCEPTANCE.md`: the desktop viewer window under a real link
conditioner (the harness drives the same switcher and decoders but not the
wgpu window), the second hardware session and full-motion frame rate on
Windows and Linux, and a two-machine run through the VPS.
