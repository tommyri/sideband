# Screen sharing: Element/MatrixRTC and LiveKit

Research date: 2026-11-06. Recommendation only; no backend migration or task-status change.

Sideband baseline: `ed377440891b15bc4e0a5104096df57a2f290c69`.
Element Call source inspected at `eef86fc06ead7c789b344e29c1385d59a1fe6614`.
Other upstream links describe the development branches/documentation inspected on
the research date, not guaranteed features of every released Element X version.
No upstream media pipeline was built or benchmarked during this investigation.
The macOS half of the decision experiment ran on 2026-11-16 (T-100); see
[Results](#results-2026-11-16-macos). Decision, 2026-11-16: keep the QUIC
backend; see [Decision](#decision-2026-11-16).

## Recommendation

Run a bounded native LiveKit comparison before implementing T-074. Favor direct
LiveKit adoption if it passes our desktop performance, packaging, permission and
mixed-network tests. Do not adopt the full Matrix stack merely for screen sharing.

The main benefit is transferring ownership of media transport, bandwidth
allocation, recovery and layered forwarding to an existing implementation. The
main costs are a separate service, libwebrtc native dependencies, frame adapters
and integration with Sideband authorization. These are engineering judgments,
not measured performance conclusions.

## What Element implements

Element Call combines Matrix call coordination with LiveKit media. Its current
web SDK's `Publisher.publish` delegates screen sharing to
`localParticipant.setScreenShareEnabled`, passing capture and publishing settings.
Stopping the share also stops its associated screen-share audio.
[Publisher source](https://github.com/element-hq/element-call/blob/eef86fc06ead7c789b344e29c1385d59a1fe6614/sdk/src/media-backend/livekit/Publisher.ts)

Capture options request system audio and browser surface switching. Shared audio
disables automatic gain control, noise suppression and voice isolation. These are
requests to the browser/platform, not guarantees of capture support on every OS.
[Capture options](https://github.com/element-hq/element-call/blob/eef86fc06ead7c789b344e29c1385d59a1fe6614/sdk/src/media-backend/livekit/publishOptions.ts)

Room options explicitly enable simulcast, adaptive streaming and Dynacast.
Screen-share bitrate/frame-rate limits and optional simulcast layers are
configurable. Adaptive streaming chooses receive quality from the attached
video element's size and visibility; Dynacast avoids producing unused layers.
This is browser integration, not a ready-made native Tauri viewer.
[Room options](https://github.com/element-hq/element-call/blob/eef86fc06ead7c789b344e29c1385d59a1fe6614/sdk/src/media-backend/livekit/livekitOptions.ts),
[rendering architecture](https://github.com/element-hq/element-call/blob/eef86fc06ead7c789b344e29c1385d59a1fe6614/sdk/SdkArchitecture.md)

The separate matrix-rust-rtc project describes a native transport abstraction,
Matrix call membership, per-participant media keys, multi-SFU coordination and
constraints for stream visibility/dimensions. Its LiveKit adapter pulls in
libwebrtc; its browser bindings leave media to livekit-js. This provides a useful
design reference, but it is substantially more than a screen-sharing library.
[Rust SDK overview](https://github.com/element-hq/matrix-rust-rtc),
[architecture](https://github.com/element-hq/matrix-rust-rtc/blob/main/ARCHITECTURE.md)

For Sideband, reuse the design ideas: hide media-backend types from product UI,
identify screen video and shared audio separately, bound frame queues, and let
window visibility/size drive subscriptions. Adopting Matrix membership, identities
and federation would duplicate or replace existing Sideband responsibilities.
Element sources carry AGPL/commercial licensing; Sideband declares MIT OR
Apache-2.0. Any direct source reuse needs a deliberate licensing decision; this
recommendation is to implement the ideas independently and use LiveKit directly.
[Element source/license notices](https://github.com/element-hq/element-call/blob/eef86fc06ead7c789b344e29c1385d59a1fe6614/sdk/src/media-backend/livekit/Publisher.ts),
[Matrix RTC licensing](https://github.com/element-hq/matrix-rust-rtc#readme)

## What LiveKit provides

The Rust SDK supports Windows/macOS/Linux and uses native libwebrtc. Its publisher
options expose simulcast, VP9/AV1 SVC, encoder selection, bitrate/frame-rate limits
and degradation preference. Screen-share defaults favor retaining resolution;
game sharing may need a different motion-versus-detail policy.
[SDK](https://github.com/livekit/rust-sdks),
[publishing options](https://github.com/livekit/rust-sdks/blob/main/livekit/src/room/options.rs)

The Rust README still leaves "Adaptive Streaming" unchecked. That does not mean
there is no network adaptation: remote publications expose quality ceilings,
requested dimensions and subscription enable/disable controls. Sideband would
wire these to native window state. The SFU has bandwidth estimation, probing and
stream allocation machinery; it can forward a lower available layer to one
subscriber without forcing every subscriber onto it.
[Receive controls](https://github.com/livekit/rust-sdks/blob/main/livekit/src/room/publication/remote.rs),
[SFU allocator](https://github.com/livekit/livekit/blob/master/pkg/sfu/streamallocator/streamallocator.go)

### Two integration paths

| Path | Preserves | Changes | Limitation |
| --- | --- | --- | --- |
| Raw frames into LiveKit | Native capture/source picker and product UI | SDK encodes, transports and decodes; native renderer needs a frame adapter | GPU copies, codec selection and hardware simulcast must be measured |
| Existing encoded H.264 into LiveKit | Capture and encoder | SDK transports; receive path needs integration | One spatial layer; does not solve independent viewer quality |

The raw-frame API accepts a screencast flag. Received native frames can be rendered
outside a webview; LiveKit provides a Rust wgpu/egui example. Our window lifecycle
is reusable, but our renderer currently accepts moq-video frame types and cannot
consume LiveKit frames unchanged.
[Video source](https://github.com/livekit/rust-sdks/blob/main/libwebrtc/src/video_source.rs),
[native rendering example](https://github.com/livekit-examples/rust-dev-client)

Pre-encoded publishing is documented from Rust SDK 0.7.53. It accepts complete
encoded access units, with capture timestamps and SPS/PPS on H.264 keyframes,
and exposes keyframe and rate-control feedback. Sideband already emits Annex-B
access units with in-band parameters. We would still need timestamp conversion,
encoder bitrate control and lifecycle adapters. This path explicitly has one
spatial layer and disables simulcast; it is a migration bridge, not a layered
quality solution. The separate `livekit-capture` helper is Developer Preview.
[Pre-encoded documentation](https://docs.livekit.io/transport/media/pre-encoded/)

### Costs and unresolved questions

- **Windows hardware encoding:** SDK platform support is documented, but the
  explicit Rust hardware matrix does not establish our Windows GPU path. Verify
  the actual encoder, copies, CPU use and 1080p60 performance before adoption.
  Linux/macOS hardware entries also require testing on the target machines.
  [Hardware matrix](https://docs.livekit.io/robotics/media/video/encoders/),
  [Rust platform list](https://github.com/livekit/rust-sdks#features)
- **Operations:** self-hosting is supported; a paid cloud service is not required.
  A separate LiveKit service adds TLS, media ports, TURN configuration and release
  management. Recording/Ingress/AI-agent services are unnecessary for our basic
  screen-sharing flow.
  [Self-hosting](https://docs.livekit.io/transport/self-hosting/),
  [ports](https://docs.livekit.io/transport/self-hosting/ports-firewall/)
- **Authorization:** Sideband must issue scoped tokens and reconcile channel
  moves, bans and permission changes. The self-hosting comparison lists token
  revocation as Cloud-only. An unexpired token must not silently restore access
  after removal; test this explicitly and design admission/expiry accordingly.
  [Deployment capability comparison](https://docs.livekit.io/transport/self-hosting/)
- **Encryption:** media E2EE is available, but key generation, distribution and
  rotation are application responsibilities. Adopting LiveKit does not import
  Matrix's identity/key-management guarantees.
  [Encryption](https://docs.livekit.io/transport/encryption/)
- **Audio:** system/application audio capture remains platform work. Keeping
  Sideband voice initially bounds the trial, but LiveKit cannot automatically
  synchronize its video to our separate QUIC voice clock. For future shared
  application audio, carry that audio and its video together and test lip sync.
- **Packaging:** libwebrtc brings native build artifacts and notices. The Rust SDK
  is Apache-2.0, but the complete dependency bundle still needs review.
  [SDK license](https://github.com/livekit/rust-sdks/blob/main/LICENSE)

## Comparison with the current implementation

Sideband already has source selection, moq-video capture/H.264 encoding, a custom
Quinn relay, bounded queues, keyframe recovery, native decoding and wgpu windows.
The transport uses one QUIC stream per GOP and one encoded rendition per sharer;
using moq-video does not mean we implement the MoQ network protocol.

Retaining this preserves deployment simplicity and full control. Continuing it
also means owning congestion policy, per-viewer quality, recovery behavior and
their cross-platform testing. T-074 currently promises that a weak viewer will
not lower other viewers' quality while excluding simulcast/SVC. With one
rendition and no transcoding, that promise needs revision or additional layering;
arbitrary packet dropping cannot manufacture a lower-resolution rendition.

Local evidence: `crates/ss-video/src/publisher.rs`,
`crates/ss-client-core/src/media/sender.rs`,
`apps/desktop/src-tauri/src/viewer_gpu.rs`, and
`docs/plan/tasks/T-074-adaptive-bitrate.md` at the Sideband baseline above.

## Proposed decision experiment

1. Keep the current backend available as the comparison baseline. Add a small
   media-backend boundary around publishing, subscriptions and frame delivery.
2. Prototype the raw-frame LiveKit path with self-hosted LiveKit, native capture
   and native presentation. Use a pinned SDK/build. Include layered publishing;
   a single-stream demo is insufficient to validate T-074's main benefit.
3. Test Windows gaming capture, macOS and Linux (including Wayland), real encoder
   selection, source closure and permission cancellation. Record latency,
   FPS, bitrate, CPU/GPU use, memory and installer size against our baseline.
4. Test one healthy and one impaired viewer, recovery, resizing/hidden windows,
   reconnects, channel moves and revocation/rejoin. Target T-074's recovery times
   but report actual results rather than asserting them from SDK capabilities.
5. Use the pre-encoded path as an optional bridge if useful. Do not count it as
   proof of layered-quality support. Decide explicitly whether Windows hardware
   performance or independent viewer quality requires further integration work.

Adopt LiveKit if those results justify the added deployment/build complexity.
Otherwise keep the current backend and narrow T-074 honestly or budget for
multiple renditions. No implementation was performed by this research, and all
hardware acceptance remains pending. GitHub Actions remain manually triggered.

## Results (2026-11-16, macOS)

These are the macOS measurements from the decision experiment. They record
evidence, not a decision; the maintainer decides in T-100. Windows gaming
capture and Linux (Wayland included) are still unmeasured; see
[the plan for them](#still-to-measure-windows-and-linux).

### Setup

- Machine: MacBook Pro, Apple M4 Pro (14 cores, 20-core GPU), 48 GB,
  macOS 27.0.1 (26A434). Capture source: the external 2560×1440 display,
  scaled to 1920×1080. Rust 1.99.1. Other desktop apps kept running, so GPU
  figures are system-wide.
- Sideband: `main` at `1c25c9a`. Baseline is the real `ss-server` relay on
  `127.0.0.1`, `ss-net` GOP streams, the encoder settings of
  `ss_video::Publisher` (moq-video 0.1.21, VideoToolbox H.264 High, 2 s GOP,
  the `Preset` bitrate) and `ss_video::decode::VideoDecoder`.
- LiveKit: Rust SDK `livekit` 0.11.0 (`libwebrtc` 0.4.1, `webrtc-sys` 0.3.54,
  prebuilt libwebrtc tag `webrtc-a545de4`, mac-arm64 release: a 277 MB zip
  that unpacks to 1.1 GB), `livekit-api` 0.10.2. Server:
  `livekit/livekit-server:v1.13.11` (image
  `sha256:25cd902de7cacbd5f1f779f6a6ac1eae20ba8f56b0935934ce6b0ab80d1e3d20`,
  85 MB), `--dev`, ports bound to `127.0.0.1`, in Docker Desktop 29.9.2's
  Linux VM. The v1.13.11 release has no macOS server binary.
- LiveKit publishing is the raw-frame path. The captured `CVPixelBuffer` is
  handed to `NativeVideoSource` with no copy; the pattern goes in as I420.
  Codec H.264, `VideoEncoderBackend::VideoToolbox`, screenshare source
  (`MaintainResolution`), dynacast on. Simulcast has three layers: 1080p30
  at the preset bitrate, 720p30 at 1.2 Mb/s, 360p15 at 0.4 Mb/s. libwebrtc
  reported `SimulcastEncoderAdapter (VideoToolbox, VideoToolbox,
  VideoToolbox)` and a VideoToolbox decoder.
- Content, identical for both backends: `pattern`, a deterministic
  screen-like picture (static gradient, text typed and scrolled one line
  every 2 s, a moving 240 px box); `pattern-hard`, full-frame motion (the
  gaming-like worst case); `screen`, live ScreenCaptureKit capture of a
  mostly static desktop. All at the 1080p30 preset (4 Mb/s).
- Each run: one publisher, two viewers, separate processes, 60 s. Medians are
  of per-second samples after the first 5 s.
- Latency method: the sender writes a 72-block barcode (capture wall-clock
  time, frame number, checksum) into the luma plane of each frame. Each
  viewer reads it from the decoded picture. All processes share the host
  clock. This is **capture-to-decoded-frame** latency. It leaves out the
  ScreenCaptureKit delivery delay before the frame reaches the app, and the
  wgpu draw plus vsync after it; both are the same for either backend. It
  is not glass-to-glass. The baseline viewer decodes as soon as a frame arrives; the
  desktop viewer's `Playout` scheduler (up to 60 ms of wait against the voice
  clock) is not included. LiveKit's figure includes libwebrtc's adaptive
  jitter buffer, which the Rust SDK does not expose.
- CPU is each process's own user plus system time per wall second (100 % is
  one core). Memory is the process's physical footprint. The LiveKit
  server's CPU and memory come from `docker stats` (inside the VM).
  During LiveKit runs, Docker Desktop's host processes used about 28 % of a
  core, against 7 % idle: roughly 21 % of a core goes to the VM and its UDP
  port forwarding. That is a cost of this setup, not of a Linux deployment.
- GPU: `ioreg` device utilisation, system-wide, was 0–14 % in every run, with
  no difference between backends. The VideoToolbox media engine is not
  visible without `powermetrics`, which needs root, so the hardware encode
  and decode load was not measured.
- The raw JSON logs are not committed. The prototype, its runner and the
  analysis script that produced these tables are in
  [`experiments/livekit-spike/`](../../../experiments/livekit-spike/README.md).
- Unlike the Mac in T-071's notes, this one opens a 1920×1080
  VideoToolbox session; both backends used the hardware encoder.

### Two healthy viewers

| Content | Backend | Encoded fps | Viewer fps | Sent kb/s | Per viewer kb/s | Latency p50 / p95 ms | Sender CPU % | Viewer CPU % | Sender / viewer MiB | Server CPU % / MiB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| pattern | QUIC | 30 | 30 | 1 800 | 1 780 | 15.6 / 24 | 13.1 | 1.7 | 19 / 9.5 | 1.5 / 28 |
| pattern | LiveKit, simulcast | 30 per layer | 30 | 1 760 (3 layers) | 1 190 | 67 / 87 | 15.8 | 2.8 | 36 / 20 | 3.7 / 39 |
| pattern | LiveKit, one layer | 30 | 30 | 2 020 | 2 020 | 68 / 94 | 12.4 | 3.4 | 27 / 21 | 3.5 / 37 |
| pattern-hard | QUIC | 30 | 30 | 4 020 | 4 030 | 17.4 / 27 | 14.6 | 1.6 | 19 / 12 | 1.6 / 29 |
| pattern-hard | LiveKit, simulcast | **≈ 11 per layer** | **11** | 5 590 | 2 160 | 107 / 137 | 16.6 | 2.2 | 37 / 17 | 5.1 / 39 |
| pattern-hard | LiveKit, one layer | 30 | 30 | 4 010 | 4 010 | 101 / 170 | 13.5 | 4.2 | 29 / 23 | 4.2 / 39 |
| screen | QUIC | 29.2 | 29.2 | 560 | 570 | 15.7 / 24 | 3.2 | 1.9 | 16 / 8 | 1.5 / 27 |
| screen | LiveKit, simulcast | 29.3 | 29.2 | 230 | 110 | 44 / 48 | 7.5 | 3.0 | 21 / 21 | 3.0 / 37 |

Sender CPU in the pattern rows includes generating the pattern, about 10 %
of a core, which costs the same for both backends. The `screen` rows show
the pipelines alone.

- **Latency:** the QUIC path delivered decoded frames 15–17 ms after
  capture. LiveKit took 44–107 ms. Most of the difference is libwebrtc's
  jitter buffer (`jitterBufferDelay` 40–70 ms on the pattern). It rose to
  about 65 ms a few seconds after each viewer joined and decayed by about
  0.5 ms a second. Setting
  the server's `room.playout_delay` to min 0 / max 0 and to min 0 / max 10
  did not change it (69 / 89 and 70 / 92 ms). A single unscripted 16 s run
  once showed 16 ms with a 0 ms jitter buffer, but the scripted runs never
  reproduced it.
- **Simulcast on full motion:** with three layers, libwebrtc's VideoToolbox
  path encoded every layer at about 11 fps on `pattern-hard`, although it was
  fed 30 frames a second, and it reported no quality limitation. One
  layer at the same 4 Mb/s held 30 fps, and so did moq-video's VideoToolbox
  session. Layered publishing is what independent viewer quality needs, and
  on gaming-like content it cost two thirds of the frame rate on this Mac.
  Windows and Linux must be checked for the same effect.
- **Bitrate:** moq-video's VideoToolbox session spends close to its average
  target (about 1.8 Mb/s on `pattern`, 4 Mb/s on `pattern-hard`). libwebrtc
  undershoots on easy content (1.2 Mb/s for the top layer) and adds about
  0.6 Mb/s of lower layers.
- **Resources:** LiveKit cost about 2–4 percentage points more sender CPU,
  1–2.5 points more viewer CPU, and about twice the memory (sender 21–37 MiB
  against 16–19, viewer 17–23 against 8–12). The LiveKit server used 3–5 %
  of a core and 37–39 MiB in the VM; `ss-server` used 1.5 % and 27–29 MiB
  for the same relay work, and also serves voice, chat and the rest.

### One healthy and one impaired viewer

`viewer2` went through a user-space UDP proxy (`impair`) that needs no root.
It was clean for 20 s, impaired for 30 s, then clean again. The baseline
viewer connected to the relay through the proxy. The LiveKit viewer was
relay-only, through a TURN server behind the same proxy, so its path differs
from the healthy viewer's only by the TURN hop. `viewer1` connected directly
in both cases.

| Impairment and content | Backend | Healthy viewer during impairment | Impaired viewer during impairment | Impaired viewer after it ends |
| --- | --- | --- | --- | --- |
| 600 kb/s, 2 % loss, +40 ms; `pattern` | QUIC | 30 fps, 15 ms, unchanged | Frozen 27 of 30 s (median 0 fps, gap up to 21 s), no lower rendition to fall back to | Full 1080p30 at 15 ms after 1.8 s |
| same | LiveKit, simulcast | 30 fps, 1080p, unchanged | A 1.8 s freeze, then the 360p15 layer after 2.7 s. One failed step up to 720p at +13 s froze it for 4 s. 6 seconds with a gap over 1 s; latency 100–190 ms | 360p for another 21 s, 720p, then 1080p30 34 s after the end. Latency stayed at 200–240 ms for about 50 s (jitter buffer) |
| same, `playout_delay` 0/0 | LiveKit, simulcast | Unchanged | 360p after 3.8 s; 12 seconds with a gap over 1 s, up to 8.8 s | Full quality 35 s after the end |
| same | LiveKit, one layer | Unchanged | Frozen 28 of 30 s, like the baseline | Full quality after 2.5 s |
| 1.5 Mb/s, 2 % loss, +40 ms; `pattern-hard` | QUIC | 30 fps, 15 ms, unchanged | 4 fps, latency about 2.7 s, 16 seconds with a gap over 1 s | Full quality after 1.8 s |
| same | LiveKit, simulcast | 11 fps (the encoder limit above), unchanged | 360p after 3.1 s, 2 seconds with a gap over 1 s, latency about 450 ms | Not back at 1080p within 50 s (720p by then) |

- Both backends kept the healthy viewer at full quality. The relay's
  per-subscriber queues already isolate viewers, so this part of T-074's
  promise holds today.
- Only layered publishing gave the slow viewer a usable picture. With one
  rendition, LiveKit froze like the baseline: the SFU cannot make a lower
  layer out of a single encode either.
- LiveKit took 21–35 s to climb back to full quality, against about 2 s for
  the baseline, and its jitter buffer kept latency raised for about a minute.
  T-074's targets are a bitrate drop within 3 s, no freeze over 1 s, and
  recovery within 30 s. Neither backend met them as configured. LiveKit
  downgraded in 2.7–3.8 s but froze and recovered slowly. The baseline has no
  sender adaptation yet, so it froze for the whole impairment.
- In one earlier baseline run, the healthy viewer stopped receiving 2 s into
  the impairment and never recovered. That run used a first version of the
  spike's demux, which stopped accepting streams after any per-stream error;
  the desktop demux skips such streams. With the desktop's rule, the rerun
  above was clean. The logs cannot rule out a relay stall, so a regression
  test with a stalled subscriber next to a healthy one is worth adding
  whichever backend stays.

### Reconnects, channel moves and revocation

| Case | QUIC (Sideband control plane) | LiveKit (token and room service) |
| --- | --- | --- |
| Lose access (leave voice / revoke `canSubscribe`) | `LeaveVoice`: the last frame arrived within one frame interval | `UpdateParticipant(canSubscribe=false)`: the last frame came 67 ms after the call |
| Regain access, to first frame | `JoinVoice` and `SubscribeTrack`: 20 ms (47 ms in a trial run) | Restore `canSubscribe`: 423 ms |
| Reconnect, to first frame | Close QUIC, resume token, join, subscribe: 1.1 s | Full reconnect: 1.46 s. Signal-only resume: no media gap |
| Channel move | Leave and join another voice channel (server-enforced, T-070 tests) | `MoveParticipant` returns `not implemented` on single-node self-hosted 1.13.11. A move is a disconnect plus a new token for the other room |
| Kick or ban | Server-enforced on the same connection (T-070 revocation tests) | `RemoveParticipant` disconnects the participant, but **the same unexpired token rejoined 2 s later** and received video. The default `revoke_token_ts` did not block it on self-hosted |
| Token expiry | Not applicable: sessions are server-side | A viewer whose 15 s token had expired made a full reconnect at 30 s and received video: the SDK reconnects with tokens the server refreshes, so expiry never ends a live session |

The research note's concern holds: with self-hosted LiveKit, Sideband would
have to enforce removal itself. That means short-lived tokens, a
server-side admission check (for example a webhook or an identity per
voice session that Sideband refuses to reissue), and `RemoveParticipant` on
every ban, kick, move and permission change.

### Packaging and build

- **Size:** a stripped release binary of the spike grew from 19.2 MB
  (baseline only, `--no-default-features`) to 42.2 MB with LiveKit:
  +23.1 MB. Compressed with `xz -9`, it grew from 4.95 MB to 11.1 MB
  (+6.2 MB), roughly what a compressed installer would gain. The delta
  includes `livekit-api` and the spike's TURN server, which a client would
  not ship. libwebrtc links statically (`libwebrtc.a` is 721 MB before
  linking), so no extra dynamic library ships.
- **Link flags:** the binary needs `-ObjC`. Without it, the first H.264
  capability query throws an `NSInvalidArgumentException` (an Objective-C
  category stripped from the static library).
- **Notices:** the libwebrtc bundle's `LICENSE.md` is 178 KB with 25
  sections, among them abseil-cpp, BoringSSL, dav1d, ffmpeg, libaom, libvpx,
  libyuv, OpenH264, Opus, protobuf and RNNoise. ffmpeg code is linked into
  the binary (404 `ff_`/`av` symbols in the spike), and its notice carries
  the LGPL-2.1 and GPL-2.0 texts. Statically linking LGPL code into an
  MIT/Apache-2.0 application needs legal review, and probably a custom
  libwebrtc build without it.
- **cargo-deny:** run with the product `deny.toml` on the spike's
  dependency graph, it rejects one crate: `progenitor-client` (MPL-2.0, from
  `livekit-api`'s room service, which would live in the server). It cannot
  see anything inside the prebuilt libwebrtc, so the notices above need a
  separate review.
- **Build:** the first build downloads 277 MB from GitHub releases; offline
  or pinned-mirror builds need `LK_CUSTOM_WEBRTC`. The spike's `target/`
  reached 7.6 GB.

### Comparison at a glance

| Question | QUIC backend today | LiveKit (raw frames, simulcast) |
| --- | --- | --- |
| Capture-to-decoded latency on loopback | 15–17 ms | 44–107 ms (adaptive jitter buffer) |
| 1080p30 on screen-like content | 30 fps | 30 fps |
| 1080p30 on full-motion content | 30 fps | about 11 fps with simulcast; 30 fps with one layer |
| Healthy viewer while another is impaired | Unaffected | Unaffected |
| Impaired viewer (600 kb/s) | Frozen for the whole impairment | 360p15 after about 3 s, some freezes |
| Recovery after impairment | About 2 s | 21–35 s to full quality |
| Sender / viewer CPU (screen capture) | 3.2 % / 1.9 % | 7.5 % / 3.0 % |
| Memory, sender / viewer | 16–19 / 8–12 MiB | 21–37 / 17–23 MiB |
| Server | In `ss-server` (1.5 %, shared process) | Second service (3–5 % in the VM, plus TURN and ports to run) |
| Binary delta | — | +23 MB stripped, +6 MB compressed |
| Licences | Already reviewed | 25 bundled notices, statically linked ffmpeg (LGPL); invisible to cargo-deny |
| Channel move | Server-side | Not implemented when self-hosted; reconnect with a new token |
| Removal | Enforced by the server | Old token rejoins; Sideband must enforce it |
| A/V sync with QUIC voice | Shared 48 kHz clock | No shared clock |

### Decision (2026-11-16)

The maintainer kept the QUIC backend. LiveKit is not adopted. On macOS its
only gain for a slow viewer came from simulcast, and that gain can be had
here with a second rendition. Its costs are certain: a second service,
libwebrtc and its licences, tokens Sideband would have to police, and no
shared clock with voice. T-074 now budgets the second rendition. The
Windows and Linux runs below are not needed for this decision. Run the
Windows game-capture row with T-071's Windows acceptance; if moq-video fails
1080p60 there while libwebrtc holds it, reopen the question.

### Still to measure: Windows and Linux

macOS cannot measure these. Each machine runs the same spike. Run the
LiveKit server on one Linux host on the same LAN as the sender
(`livekit/livekit-server:v1.13.11 --dev --node-ip <LAN IP>`), or keep it on
the sender. Record which, because the Docker Desktop VM hop seen here does
not exist on Linux.

Port the spike first (about a day). It needs a Windows capture path
(Windows.Graphics.Capture through moq-video, which gives D3D11 NV12) and a
Linux PipeWire path (moq-video `pipewire`, which gives DMA-BUF). On Windows,
LiveKit's `NativeBuffer` takes no D3D11 texture, so frames go in as I420 or
NV12 after a GPU readback: count that copy. On both, the barcode reader needs
a mapped or downloaded luma plane.

| Machine | Steps | Record |
| --- | --- | --- |
| Windows 11, NVIDIA RTX (driver ≥ 550), 1080p and 1440p monitors. A game in borderless fullscreen (GPU-bound, 60+ fps), plus a desktop/IDE window | `base-hard`/`lk-hard`-style runs at 1080p60 (`--preset p1080p60`) with game capture for 120 s. Then `*-pattern`, `*-impair` and `*-life`. Repeat LiveKit with `--auto-encoder`, and with NVENC requested (`VideoEncoderBackend::Nvenc`) | `encoder_implementation` per layer (MF H.264, NVENC or OpenH264 fallback), sender fps per layer, `qualityLimitationReason`, game fps with and without sharing (PresentMon), sender CPU and GPU (Task Manager GPU engine 3D/Video Encode, or `typeperf "\GPU Engine(*)\Utilization Percentage"`), VRAM, barcode latency p50/p95, viewer decoder, installer size delta (MSI/NSIS) |
| Windows 11, Intel iGPU or AMD | The same 1080p60 game run | Whether the libwebrtc H.264 path finds hardware at all (it may fall back to OpenH264 in software), CPU at 1080p60 |
| Linux, GNOME on Wayland (Fedora 41+ or Ubuntu 24.04), Intel or AMD with VA-API | Portal picker with `screen` content. Run `*-pattern`, `*-screen`, `*-impair`, `*-life`. Cancel the portal dialog once. Close the shared window mid-run | Whether libwebrtc uses VA-API (`VideoEncoderBackend::Vaapi`) or software, fps per layer, CPU, `intel_gpu_top`/`radeontop`, DMA-BUF zero copy or readback, portal-cancel and source-close behaviour on both backends, `.deb`/AppImage size delta |
| Linux, KDE Plasma on Wayland, NVIDIA proprietary driver | The same, plus NVENC | Wayland plus NVIDIA is the riskiest case: record failures verbatim |
| Linux X11 (any) | `*-pattern`, `*-screen` | X11 capture through both backends |

For every run, keep the raw JSON logs, the machine (CPU, GPU, driver, OS
build, display mode), the binary or installer size, and the exact versions.
Use two viewers on separate machines for latency, with a high-speed camera
filming the sender screen and a viewer screen side by side for a real
glass-to-glass figure. The barcode figure here leaves out capture and display.

### The media-backend boundary (described, not built)

No product code changed. A real boundary means splitting
`ss_video::Publisher`, which fuses capture, scaling and encoding, because
LiveKit encodes inside libwebrtc. It also means changing the desktop share
and viewer loops. That is a refactor across four crates, not a small seam,
so the spike calls both backends from `experiments/livekit-spike/` instead.
If LiveKit is adopted, this is the shape:

```rust
/// Raw frames in. The backend encodes (LiveKit) or encodes and frames GOPs (QUIC).
trait ScreenPublisher {
    async fn push(&mut self, frame: moq_video::Frame) -> Result<()>;
    fn request_keyframe(&mut self);          // QUIC: viewer RequestKeyframe. LiveKit: PLI is internal, so a no-op.
    fn demand(&self) -> SubscriberHint;      // QUIC: server aggregate. LiveKit: dynacast pauses unused layers.
    fn stats(&self) -> PublishStats;         // per layer: encoder, fps, bitrate, limitation reason
    async fn stop(self) -> Result<()>;
}

/// Decoded pictures out, in the surface the renderer already draws.
trait ScreenSubscription {
    async fn next(&mut self) -> Result<Option<moq_video::Frame>>;
    fn prefer(&self, max_height: u32, max_fps: u32); // QUIC: SetPreferredQuality. LiveKit: set_video_quality / update_video_dimensions.
    fn set_visible(&self, visible: bool);            // LiveKit: set_enabled. QUIC: unsubscribe.
    fn stats(&self) -> ViewStats;
}
```

Where it would cut:

- `crates/ss-video/src/publisher.rs`: split into a capture source and an
  encoder stage. Only the QUIC backend uses the encoder stage.
- `crates/ss-client-core/src/media/sender.rs` (`ScreenShareSession`), with
  `media/mod.rs` and `media/viewer.rs`: the QUIC implementations of the two
  traits.
- `apps/desktop/src-tauri/src/stream.rs` (the `Publisher` loop) and
  `viewer.rs` (`Playback`, which decodes from `GroupReader`): call the traits
  and stop naming `GroupReader` or `VideoDecoder`. `viewer_gpu.rs` already
  draws `moq_video::Frame`, so it does not change.
- The frame adapter: a LiveKit `NativeBuffer` wraps a `CVPixelBuffer` on
  macOS, so it maps to `Surface::PixelBuffer` with a retain and no copy. On
  Windows and Linux the Rust SDK hands out I420, which costs one CPU upload
  per frame into the wgpu renderer.
- The server: a control message that issues a LiveKit token for the
  caller's current voice channel, and permission-refresh hooks that call the
  LiveKit room service.
