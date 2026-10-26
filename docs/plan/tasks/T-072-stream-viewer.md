# T-072 — Client: stream viewer (decode + wgpu window)

Status: todo
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
