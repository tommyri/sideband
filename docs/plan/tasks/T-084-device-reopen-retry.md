# T-084 — Retry failed device reopens during a call

Status: todo
Phase: 3
Size: S
Depends on: T-010, T-041, T-054, T-083
Paths: `crates/ss-client-core/src/voice/worker.rs`, `crates/ss-audio/src/device/manager.rs`, `crates/ss-audio/examples/`

## Goal
Unplugging and replugging a headset recovers audio without leaving the
channel, as `docs/design/voice.md` already promises.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
reproduced on hardware. T-083 touches the same worker file; run this after.

## Scope
- On `Lost`, the worker sets `devices.capture = None` and calls
  `open_capture(Some(id))`. If that fails the stream stays `None`, and the
  reopen flags are only computed while a stream is `Some`, so a later
  `Added` or `DefaultChanged` does nothing. Playback has the same shape, and
  VoiceProcessingIO is set to `None` before its fallible open. Keep a
  "wanted but closed" state per direction; retry on a timer and on
  `Added`/`DefaultChanged`; fall back to the default device when the
  selected one is gone and return to it when it reappears.
- Re-applying the same settings does not help today because `configure`
  reopens only when ids differ. Make Apply force a reopen of a closed
  stream.
- `render_tap.push` runs on every render but is cleared only in
  `capture_once`. With no capture stream it grows without bound (reported
  about 384 KB/s) and the first resumed frame hands the backlog to the AEC.
  Cap it at `MAX_RENDER_BATCH`.
- Report the degraded state to the UI (device missing, using fallback) and
  clear it on recovery. The loopback example already retries every 500 ms;
  share that policy.

## Out of scope
Device selection UI changes beyond showing the state.

## Acceptance criteria
- Test with a mock device manager: selected capture device disappears,
  reopen fails, device returns; the stream is reopened within one second
  and memory use stays flat while it is closed.
- Same for playback and for VoiceProcessingIO mode on macOS.
- Manual check recorded for USB headset unplug/replug on macOS; Windows
  and Linux rows stay open for T-011/T-012.

## Notes from implementation
