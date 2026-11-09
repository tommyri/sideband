# T-100 — Decide the screen-share backend before adaptive bitrate

Status: todo
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
