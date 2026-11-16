# T-100 — Decide the screen-share backend before adaptive bitrate

Status: done
Phase: 5
Size: M
Depends on: T-070, T-071, T-072
Paths: `docs/plan/notes/screenshare-livekit-research.md`, `docs/plan/tasks/T-074-adaptive-bitrate.md`, `docs/design/decisions.md`, `crates/ss-video/`, `crates/ss-client-core/src/media/`

## Goal
A recorded decision: keep the custom QUIC video transport or adopt
LiveKit, made from measurements, so T-074 is specified against the backend
that will exist.

Source: `docs/plan/notes/screenshare-livekit-research.md` (2026-11-06)
recommends this experiment and no task tracked it. The 2026-11-09 review
found T-074 promises something its own scope rules out.

## Scope
- T-074 says a streamer with one slow viewer keeps full quality for the
  others, and excludes simulcast and SVC. With one encoded rendition and a
  server that never transcodes, the only lever is the sender's bitrate,
  which affects everyone. Until this task decides, T-074 depends on it.
- Run the experiment the research note describes:
  - a small media-backend boundary around publishing, subscription and
    frame delivery, with the current backend as the baseline;
  - a raw-frame LiveKit prototype with self-hosted LiveKit, native capture
    and native presentation, pinned SDK, layered publishing included;
  - Windows gaming capture, macOS and Linux (Wayland included): latency,
    frame rate, bitrate, CPU/GPU, memory and installer size against the
    baseline;
  - one healthy and one impaired viewer, recovery, hidden windows,
    reconnects, channel moves and permission revocation.
- Weigh the costs the note lists: a second service to deploy, libwebrtc
  native dependencies and notices, frame adapters, mapping Sideband
  authorization onto LiveKit tokens, and no shared clock with the QUIC
  voice path.
- Decide, then either:
  - keep the current backend and rewrite T-074 honestly (sender-wide
    adaptation only, or budget a second rendition), or
  - adopt LiveKit and open the migration tasks.
- Update D3 and the "screen sharing must fit the transport we have"
  section of `docs/design/decisions.md`.

## Out of scope
The migration itself. Screen-share audio.

## Acceptance criteria
- The research note gains a results section with the measurements, the
  machines used and the decision.
- T-074's scope and acceptance criteria match the chosen backend.
- `docs/design/decisions.md` records the decision and its reasons.

## Notes from implementation

2026-11-12: the maintainer chose to run the LiveKit experiment before
deciding (T-106 round), rather than keep the QUIC backend with one stream or
add a second rendition now. The prototype and the macOS measurements can be
done here; the Windows gaming-capture and Linux (Wayland) measurements need
those machines. T-074 stays blocked until the results are in.

2026-11-16: macOS half of the experiment, done in
`experiments/livekit-spike/` (outside the product workspace; no product code
changed). The full results are in the research note's
[Results](../notes/screenshare-livekit-research.md#results-2026-11-16-macos).
Machine: M4 Pro MacBook Pro, macOS 27.0.1. Versions: LiveKit Rust SDK
`livekit` 0.11.0 (prebuilt libwebrtc `webrtc-a545de4`), server
`livekit/livekit-server:v1.13.11` in Docker, `--dev`. Same frames for both
backends: the deterministic pattern, a full-motion pattern, and live
ScreenCaptureKit capture.

Summary of what was measured:

- **Latency** (capture to decoded frame, from a barcode in the luma plane):
  15–17 ms on the QUIC path, 44–107 ms on LiveKit. Most of LiveKit's figure
  is libwebrtc's adaptive jitter buffer. The server's `playout_delay`
  setting did not reduce it.
- **Frame rate:** both held 1080p30 on screen-like content. On full-motion
  content, LiveKit with three simulcast layers fell to about 11 fps per
  layer. One layer, and the current backend, held 30 fps.
- **Healthy plus impaired viewer** (600 kb/s, 2 % loss, +40 ms for 30 s):
  the healthy viewer stayed at full quality on both. The impaired viewer
  froze for the whole impairment on the current backend, and on LiveKit with
  one layer. With simulcast, LiveKit moved it to 360p15 after about 3 s, with
  a few multi-second freezes. It took 21–35 s to climb back to full quality,
  against about 2 s on the current backend.
- **Resources:** LiveKit cost a few points more CPU on sender and viewer and
  about twice the memory. The second service used 3–5 % of a core here.
  The binary grew by 23 MB stripped (6 MB compressed), and the build needs
  `-ObjC`.
- **Lifecycle:** a LiveKit resume had no media gap; a full reconnect took
  1.46 s to first frame, against 1.1 s for a QUIC reconnect. Revoking
  `canSubscribe` stopped frames in 67 ms. `MoveParticipant` is not
  implemented on self-hosted single-node LiveKit. After `RemoveParticipant`,
  the same unexpired token rejoined; an expired token did not stop a full
  reconnect either.
- **Licences:** the libwebrtc bundle carries 25 notices and statically links
  ffmpeg (LGPL-2.1, with GPL text in its notice). cargo-deny cannot see it.
  `livekit-api` brings in `progenitor-client` (MPL-2.0).
- **Not measured here:** Windows gaming capture, Linux (Wayland included),
  GPU media-engine load (needs root), and glass-to-glass latency. The
  research note has the machine-by-machine plan.

Recommendation for the maintainer: keep the QUIC backend, rewrite T-074 to
budget a second rendition, and do not open LiveKit migration tasks now. The
decision stays with you.

- The measured benefit of LiveKit is narrow. Healthy viewers are already
  isolated by the relay's per-subscriber queues. What LiveKit adds is a
  lower layer for the slow viewer, and that came from simulcast, not from
  the SFU: with one layer, LiveKit froze exactly like the baseline. The same
  benefit is reachable here by publishing a second, low rendition (for
  example 360p15 or 720p30, the layers used in this test) as its own GOP
  stream. The relay or the viewer switches at a keyframe boundary, which
  one-stream-per-GOP already makes clean. Adding sender-wide bitrate
  adaptation from `SubscriberStats` covers the rest of T-074.
- The costs are certain and fall on parts Sideband controls today:
  - a second service to deploy and secure: TLS, UDP media ports, TURN;
  - 1.1 GB of prebuilt libwebrtc downloaded at build time, the `-ObjC` link
    flag, and 25 bundled notices, including statically linked LGPL ffmpeg
    that needs legal review or a custom libwebrtc build;
  - frame adapters (free on macOS, an I420 upload per frame on Windows and
    Linux);
  - Sideband authorization mapped onto tokens that self-hosted LiveKit does
    not revoke. Moves, kicks, bans and permission changes would each need a
    `RemoveParticipant` and a server-side admission check, because a removed
    participant's old token got back in;
  - no shared clock with the QUIC voice path, so screen video would lose the
    48 kHz alignment that T-072 relies on.
- Two LiveKit results cut against its main selling point. Simulcast lost
  two thirds of the frame rate on full-motion content, which is the
  gaming-share case. And its recovery took more than 30 s, outside T-074's
  target.
- Two outcomes from the Windows and Linux runs should reopen this:
  moq-video's encoder path failing 1080p60 game capture while libwebrtc's
  holds it, or the second-rendition work in T-074 growing larger than the
  LiveKit integration costs listed above. Running the Windows game-capture
  row of the plan first answers the first question cheaply, because it also
  covers T-071's own outstanding Windows acceptance.

Remaining before `done`: the maintainer's decision, the Windows and Linux
measurements if wanted, then T-074's rewrite and the D3 update.

2026-11-16, decision: the maintainer kept the QUIC backend. LiveKit is not
adopted and no migration tasks are opened. The Windows and Linux runs were
not required for the decision.
- T-074 is rewritten around a second, low rendition chosen per subscriber
  by the relay, plus sender-wide bitrate adaptation.
- `docs/design/decisions.md` records the decision under D3 and the
  screen-sharing rules.
- The research note's results end with the decision.
- The spike stays in `experiments/livekit-spike/` as T-074's measurement
  harness and for a possible re-run.
- Reopen the question if the Windows game-capture run (do it with T-071's
  Windows acceptance) shows moq-video failing 1080p60 where libwebrtc holds
  it. Also reopen it if T-074's second rendition grows larger than the
  LiveKit costs above.
