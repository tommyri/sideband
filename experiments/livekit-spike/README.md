# LiveKit spike (T-100)

Throwaway prototype for [T-100](../../docs/plan/tasks/T-100-screen-share-backend-decision.md).
It sends the same frames through two screen-share backends on one Mac and
measures both the same way. Results are in
[the research note](../../docs/plan/notes/screenshare-livekit-research.md#results-2026-11-16-macos).

This is not product code. It has its own `[workspace]` and `Cargo.lock`. It
is not a member of the product workspace, so `just check`, clippy and
`cargo deny` never see it. It reads the product crates by path and leaves
them unchanged.

## What it contains

| Backend | Path in the spike |
| --- | --- |
| Baseline (Sideband today) | The real `ss-server` relay on `127.0.0.1` (`baseline-server`). `ss-net` GOP streams. Encoder settings copied from `ss_video::Publisher` (moq-video VideoToolbox, H.264 High, 2 s GOP, `Preset` bitrate). `ss_video::decode::VideoDecoder`. Newest-pending-GOP demux, as the desktop viewer does. |
| LiveKit | `livekit` 0.11.0 raw-frame path. `NativeVideoSource` takes the captured `CVPixelBuffer` with no copy, or I420 for the pattern. libwebrtc encodes H.264 with VideoToolbox: three simulcast layers (1080p30 at the preset bitrate, 720p30 at 1.2 Mb/s, 360p15 at 0.4 Mb/s), dynacast on. `NativeVideoStream` delivers decoded frames. The server is `livekit/livekit-server:v1.13.11` in Docker, `--dev`, bound to `127.0.0.1`. |

Both backends take frames from one `FrameSource` (`src/source.rs`):

- `pattern` is a deterministic, screen-like picture: a static gradient,
  text that gets typed and scrolls one line every 2 s, and a moving box;
- `pattern-hard` is full-frame motion, the worst case;
- `screen:<id>` is native ScreenCaptureKit capture, scaled to 1920×1080.

Every frame carries a 72-block luma barcode with the capture wall-clock time
(`src/stamp.rs`). Viewers read it back from the decoded picture, which gives
the capture-to-decoded-frame latency on one shared clock.

Other roles:

- `impair` is a user-space UDP proxy. It adds a rate limit with a 200 ms
  queue, random loss and delay on a timed schedule, and needs no root;
- `turn` is a static-credential TURN server (webrtc-rs). A relay-only LiveKit
  viewer reaches it through `impair`, so only that viewer is impaired. The
  baseline viewer connects through `impair` directly.

## Running it

macOS on Apple silicon, Docker Desktop, and Screen Recording permission for
the terminal (only the `screen` scenarios need it).

```sh
cd experiments/livekit-spike
cargo build --release      # first build downloads libwebrtc (~277 MB zip, 1.1 GB unpacked)
docker pull livekit/livekit-server:v1.13.11
./target/release/livekit-spike sources              # display ids for SCREEN=screen:<id>
./run.sh /tmp/t100 base-pattern lk-pattern          # each scenario is about 60-100 s
python3 -I analyze.py /tmp/t100
docker rm -f t100-livekit                           # the runner leaves the last container running
```

Scenarios (`run.sh`):

| Scenario | What |
| --- | --- |
| `base-pattern`, `lk-pattern`, `lk-pattern-single` | One publisher, two healthy viewers, screen-like pattern. `single` turns simulcast off. |
| `lk-pattern-lowlat`, `lk-pattern-lowlat10` | As `lk-pattern`, with LiveKit's `room.playout_delay` set to min 0 / max 0 or max 10 ms. |
| `base-hard`, `lk-hard`, `lk-hard-single` | Full-motion pattern. |
| `base-screen`, `lk-screen` | Real capture of `$SCREEN` (default `screen:1`). |
| `base-impair`, `lk-impair`, `lk-impair-lowlat`, `lk-impair-single` | `viewer2` goes through `impair`: 20 s clean, then 30 s at 600 kb/s with 2 % loss and 40 ms delay, then clean. `viewer1` stays healthy. |
| `base-impair-hard`, `lk-impair-hard` | The same with the full-motion pattern at 1.5 Mb/s. |
| `base-life` | `viewer1` leaves voice at 15 s, rejoins at 25 s, and drops and resumes its QUIC connection at 40 s. |
| `lk-life` | `viewer1` gets a signal reconnect (resume), a full reconnect, canSubscribe revoked and restored, MoveParticipant to another room and back, and RemoveParticipant followed by a rejoin with the old, unexpired token. |
| `lk-ttl` | `viewer2` holds a 15 s token and does a full reconnect at 30 s. |

Each role prints one JSON line per second (fps, kbps, latency p50/p95/max,
largest frame gap, its own CPU and memory footprint) plus event lines. `run.sh`
also samples GPU device utilisation (`ioreg`) and `docker stats` for the
LiveKit container.

To get the binary-size delta, build without LiveKit:
`cargo build --release --no-default-features --target-dir target/baseline-only`.

## Notes

- `build.rs` passes `-ObjC` to the linker. Without it, libwebrtc's
  Objective-C categories are stripped, and the first H.264 capability query
  throws `+[NSString stringForStdString:]: unrecognized selector`.
- `NativeBuffer::from_cv_pixel_buffer` consumes one reference
  (`webrtc-sys` calls `CVPixelBufferRelease`), so the spike passes a retained
  clone.
- The baseline registers only four users (`publisher`, `viewer1..3`). The
  server allows five registrations per IP per window. Clients then use resume
  tokens, which are not rate-limited.
- The baseline viewer's demux follows the desktop's rule (ss-client-core
  `media/viewer.rs`): it stops only when the connection closes, skips a
  stream whose prefix fails or takes over 1 s, and lets only a newer group id
  replace the pending group. A first version stopped on any per-stream error.
- GPU figures come from `ioreg` and are system-wide. Measuring the
  VideoToolbox media engine needs `powermetrics`, which needs root.
