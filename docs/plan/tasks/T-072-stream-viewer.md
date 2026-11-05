# T-072 — Client: stream viewer (decode + wgpu window)

Status: in-progress
Phase: 5
Size: M
Depends on: T-070, T-071
Paths: `crates/ss-video/src/decode.rs`, `apps/desktop/src-tauri/src/viewer.rs`

## Goal
Watch a friend's stream in a pop-out window with low latency and correct
A/V alignment with their voice.

## Scope
- Decoder via `moq-video` decode backends (VideoToolbox, Media Foundation/
  DXVA, VAAPI, openh264 fallback) producing GPU surfaces where possible.
- Viewer window: a native `wgpu` surface in a Tauri-managed window (or a
  separate `winit` window owned by `src-tauri`), aspect-fit, fullscreen
  toggle, always-on-top option, FPS/bitrate overlay toggle.
- Playout: minimal buffering (one group ahead), drop late frames, request a
  keyframe on decode error or after a reset; show "reconnecting stream" state.
- Lip-sync: align video timestamps to the sharer's audio track clock within
  ±60 ms using the shared 48 kHz timestamp base; measure and expose skew.
- Multiple viewers (watching two streams) supported by opening two windows.

## Out of scope
In-webview rendering (documented as not viable on WebKitGTK), recording.

## Acceptance criteria
- Glass-to-glass latency LAN < 150 ms, via VPS < 300 ms (measured with a
  timer on screen).
- Decoder failure → automatic recovery without user action.

## Notes from implementation


Implemented the native viewer backend and desktop command bridge:
- `open_stream_viewer({ serverId, trackId })` opens a Tauri-managed native
  GPU window; `stream_viewers()` returns per-window diagnostics. T-073 owns
  the user-facing Watch/source-picker controls. No video crosses the webview.
- A connection-wide QUIC demultiplexer routes independent screen tracks to
  up to eight viewers, retaining only the newest pending GOP per track.
  Duplicate viewers are rejected. Closing unsubscribes; disconnect, track
  removal, permission loss, and leaving the voice channel end the viewer.
- `moq-video` chooses the native decoder with OpenH264 fallback. Decode
  errors switch to software recovery; stream resets discard references and
  request a new keyframe. Recovery requests are throttled. A native overlay
  shows reconnecting state, decoder, FPS, bitrate, estimated skew and drops.
- Aspect-fit GPU presentation, fullscreen (F11), always-on-top and statistics
  toggle (Cmd/Ctrl+I) are available through each viewer's View menu.
- Sender video timestamps are anchored to the voice capture clock; receivers
  schedule against the sharer's mixed-audio clock, discard frames over 60 ms
  late, and fall back to sender cadence when audio is absent. Reported skew
  excludes device output latency and is not physical lip-sync certification.

Validation: `just check` (workspace formatting, clippy and tests) and the
`ss-client-core/screen-share` feature lint passed. Software H.264
roundtrip/reset and scheduling tests, audio-clock
wrap/quantum regression, two-publisher QUIC demultiplexing and subscription
cleanup, native aspect-fit tests. The macOS native smoke example rendered 296
frames through VideoToolbox and wgpu without a webview:
`cargo run -p socialspeak-desktop --example viewer_smoke`.

Remaining acceptance: installed Windows/Linux backend and window-control
checks, two simultaneous native windows, real decoder/network interruption
recovery, physical A/V skew within ±60 ms, and timer-measured LAN/VPS latency.
Run these after T-073 exposes Watch and sharing controls. Task stays
in-progress until the measured cross-platform evidence is recorded; the
implemented backend unblocks T-073.
