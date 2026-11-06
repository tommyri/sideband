# Screen sharing: Element/MatrixRTC and LiveKit

Research date: 2026-11-06. Recommendation only; no backend migration or task-status change.

Sideband baseline: `ed377440891b15bc4e0a5104096df57a2f290c69`.
Element Call source inspected at `eef86fc06ead7c789b344e29c1385d59a1fe6614`.
Other upstream links describe the development branches/documentation inspected on
the research date, not guaranteed features of every released Element X version.
No upstream media pipeline was built or benchmarked during this investigation.

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
