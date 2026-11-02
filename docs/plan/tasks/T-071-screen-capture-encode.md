# T-071 — Client: screen/window capture and hardware H.264 encode

Status: in-progress
Phase: 5
Size: L
Depends on: T-070, T-041
Paths: `crates/ss-video/` (new crate), `crates/ss-client-core/src/media/sender.rs`

## Goal
Share a display or window at up to 1080p60 with hardware encoding on all
three OSes.

Split suggestion: (a) crate + capture per OS, (b) encoder backends,
(c) pipeline + sender integration.

## Scope
- New crate `ss-video` depending on `moq-video` with `capture` and codec
  features (`vaapi`/`pipewire` on Linux) **used as a library only** (no moq
  transport). If `moq-video`'s encoder API proves coupled to its transport,
  fall back to `pinray` for capture plus per-platform encoders
  (VideoToolbox via `objc2`, Media Foundation via `windows`, VAAPI/`openh264`).
  Record the outcome in notes.
- Source picker data: displays and windows with thumbnails; macOS Screen
  Recording permission flow; Wayland portal dialog with restore token.
- Pipeline: capture → (GPU) convert/scale → H.264 encoder (low-latency
  rate control, no B-frames, IDR interval 2 s, forced keyframe on request)
  → packetize into groups → `VideoTrackSender` (T-070).
- Presets: 1080p60 8 Mb/s, 1080p30 4 Mb/s, 720p30 2 Mb/s; initial pick from
  `SubscriberHint`.
- Cursor capture toggle; stop on source closed.
- Encoder stats (fps, bitrate, encode ms, backend name) into diagnostics.

## Out of scope
Audio capture of the shared app (backlog), AV1/HEVC (backlog; HEVC where
hardware exists is a cheap follow-up), camera.

## Acceptance criteria
- Manual matrix: macOS (VideoToolbox), Windows (Media Foundation/NVENC),
  Linux X11 + Wayland (VAAPI or openh264 fallback) each sustain 1080p30 with
  < 15 ms encode time and < 25 % of one core (software fallback allowed to
  exceed).
- Keyframe request produces an IDR within 100 ms.

## Notes from implementation
- Added `ss-video` around `moq-video` 0.1.18 as a media library only. It uses
  native source discovery/capture, resize/conversion, VideoToolbox on macOS,
  Media Foundation on Windows, VAAPI/NVENC where enabled on Linux, and
  OpenH264 as the portable fallback. No MoQ transport types are used.
- Source picker metadata covers displays, windows, RGBA thumbnails, cursor
  selection, explicit permission failures, and the Wayland system picker.
  The upstream portal backend retains its restore token for the process.
- `ScreenShareSession` integrates capture/encode with `StartStream`, one QUIC
  stream per IDR-led GOP, keyframe requests, subscriber hints, source-close
  cleanup, and reconnect termination. Encoder diagnostics expose backend,
  frame/byte totals, measured fps/bitrate, and current/max encode time.
- Unit tests cover preset selection, Annex-B IDR detection, 48 kHz timestamps,
  thumbnails, and forced OpenH264 IDRs. Ignored hardware tests exercise
  VideoToolbox keyframes and the 1080p30 encode-time budget when the current
  Mac exposes a capable hardware session.
- Remaining before `done`: run the interactive source picker and the manual
  1080p30 CPU/latency acceptance matrix on macOS, Windows, Linux X11, and
  Linux Wayland. This development Mac accepts a small VideoToolbox session
  but rejects a 1920×1080 session (`VTCompressionSessionCreate -12908`), so it
  cannot supply the macOS performance measurement.
