# T-011 — Windows audio path verification (WASAPI shared mode, IAudioClient3)

Status: todo
Phase: 1
Size: S
Depends on: T-010
Paths: `crates/ss-audio/src/device/windows.rs` (only if needed), `docs/plan/notes/audio-windows.md`

## Goal
Confirm the Windows capture/playback path achieves ≤ 20 ms device latency in
shared mode and plays well with games, or implement the minimal fix.

## Scope
- Measure round-trip latency of the T-010 loopback example on Windows 11 with
  (a) onboard audio, (b) a USB headset.
- Check whether cpal's WASAPI backend uses `IAudioClient3::InitializeSharedAudioStream`
  with the minimum engine period. If not, prototype a direct path with the
  `windows` crate behind feature `wasapi-lowlatency` and compare.
- Set `AudioCategory_Communications` (via `IAudioClient2::SetClientProperties`)
  on both streams and document the effect of the Windows "communications
  activity" ducking setting; decide default (likely leave category as
  `Communications` and tell users how to disable ducking).
- Raise the audio threads with MMCSS "Pro Audio" if cpal does not.
- Verify exclusive mode is **not** used.

## Out of scope
Echo cancellation, device UI.

## Acceptance criteria
- `docs/plan/notes/audio-windows.md` with measured numbers, what cpal does
  today, and the decision.
- If a fix was needed, it's behind a feature flag with a test that the stream
  opens on CI's Windows runner (no audio device: must fail gracefully).

## Notes / references
- Microsoft: "Low Latency Audio" and "Exclusive-Mode Streams" docs recommend
  evaluating `IAudioClient3` shared mode before exclusive mode.

## Notes from implementation
