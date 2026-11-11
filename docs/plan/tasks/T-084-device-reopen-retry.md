# T-084 — Retry failed device reopens during a call

Status: awaiting-acceptance
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

### Findings, confirmed by tests written first
The worker took `DeviceManager` and the stream types directly, so the first
step adds a seam with no change in behaviour (`voice/host.rs`): a `Host`
trait for the device manager, small stream traits, and a scripted mock host.
The new tests then ran against the unchanged worker logic:
- Lost device, reopen fails: one reopen after `Lost` and none after it
  (2 opens in 1.3 s, counting the one at start). When the device came back,
  with or without an `Added` / `DefaultChanged` event, the stream stayed
  closed for the full 2 s. This held for capture and playback, and for a
  selected device and the default.
- No fallback: a missing selected device left the stream closed while the
  default device was present.
- Apply with the same settings left the stream closed (`configure` reopens
  only when the ids differ).
- Render tap: 102 frames held after 1 s with capture closed. That is
  3,840 bytes per 10 ms, about 384 KB/s, as the review said.
- VoiceProcessingIO: it was set to `None` before the open failed, and was
  never tried again.
- Not in the review (found by reading the code, then fixed and tested): a
  device change in Apply that fails, after which the previous devices also
  fail to open, set `runtime.devices = None`. After that no device event
  was read and nothing recovered.

### What changed
- `ss-audio/src/device/recovery.rs` (new): `Recovery` holds the policy for
  one direction. It tracks the device a stream is on and returns the next
  `Attempt`. It holds no streams. Exports `Recovery`, `Attempt`, `Route`
  (`Wanted` / `Fallback` / `Missing`) and `RETRY_INTERVAL`. The loopback
  example now uses it instead of its own 500 ms loop.
- Retry policy. A stream that is wanted but closed is retried:
  - at once after `Lost`;
  - at once on any `Added` that fits the direction, and on any
    `DefaultChanged`;
  - every `RETRY_INTERVAL` = 500 ms, the old loopback interval. This is
    also the inventory poll rate, so a device that returns without an
    event is found within about one second.

  Settings Apply (`retry_now`) triggers an immediate attempt. Retries run
  for the whole call; there is no backoff or limit, because a retry is one
  failed lookup on the setup thread.
- Fallback policy. With a selected device, an attempt tries that device and
  then the host default. While on the default (`Fallback`), the selected
  device is tried every 500 ms and on each `Added`, and the fallback stream
  stays open if that fails. A stream on the default (no selection, or the
  fallback) follows a `DefaultChanged`. A selected device ignores default
  changes. The new stream opens before the old one is dropped, so a
  replacement leaves no gap. Startup still opens the configured devices
  exactly, and fails as before. A device change in Apply also opens
  exactly, then falls back to the previous settings. If those do not fully
  open, the missing directions are retried (`open_available`).
- VoiceProcessingIO follows the system defaults, so it has no fallback.
  - Its unit is dropped on `DefaultChanged` or when it reports stopped.
  - It is reopened on the worker: at once, on `Added`/`DefaultChanged`, and
    every 500 ms while closed.
  - The unit is not `Send`, so each failed retry stalls the worker briefly,
    as Apply already did for this backend. No audio flows then:
    the unit carries both directions.
- `render_tap` keeps only the newest `MAX_RENDER_BATCH` (8) frames after
  each render. It is cleared when a capture stream is swapped in.
- UI: new `VoiceDeviceState::Routes { capture, playback }`
  (`DeviceRoute` = `ss_audio::device::Route`). It is sent once when the
  call's devices open and on every change. A loss that is replaced at once
  never reports `Missing`: routes are reported after the reopen attempt.
  `Unavailable` / `PermissionDenied` is sent once per outage, not on every
  retry. Desktop: `VoiceDevicePayload::Routes` with `DeviceRoutePayload`.
  The voice dock shows one status line, for example "Microphone
  disconnected. Reconnecting…" or "Microphone unavailable; using the system
  default.". The line is empty once both routes are `wanted`.

### Tests
- `voice/worker.rs` `tests::recovery`, with the mock host. Real time, cases
  in parallel, about 1.6 s:
  - `a_lost_device_reopens_within_a_second_of_returning`: capture and
    playback, selected or default, with and without an `Added` event. While
    the device is gone there are 2–5 retries in 1.3 s. It reopens within
    1 s. Routes `Wanted → Missing → Wanted`, and one notice.
  - `a_closed_capture_keeps_the_render_tap_bounded` (≤ 8 frames over 1 s).
  - `a_missing_selected_device_falls_back_to_the_default_and_returns`:
    fallback within 1 s and back within 1 s, with and without `Added`.
    Routes `Wanted → Fallback → Wanted`, and no notice.
  - `voice_processing_reopens_within_a_second_of_its_devices_returning`
    (VoiceProcessingIO mode with a mock unit): retried while gone, render
    tap bounded, reopens within 1 s.
  - `apply_reopens_a_closed_stream_with_the_same_settings`.
  - `a_failed_device_change_keeps_retrying_the_previous_devices`.
- `ss-audio` `device::recovery` unit tests: timer and event retries,
  fallback and return, following the default, and Apply waking only a
  degraded stream.
- Vitest: `deviceRouteText` in `features/voice/api.test.ts`.
- `just check` passes. `cargo clippy -p socialspeak-desktop --all-targets
  -- -D warnings` and `cargo clippy -p ss-client-core --all-targets -- -D
  warnings` (no `macos-voiceio`) are clean. `tsc -b`, `eslint .`,
  `prettier --check .` and `vitest run` pass in `apps/desktop`, and the
  lockfile is unchanged.

### Files outside the Paths
- `crates/ss-audio/src/device/mod.rs`: declares and re-exports `recovery`.
- `crates/ss-audio/src/device/watch.rs`: `DeviceId::new` is public, so the
  mock host (and other hosts) can name devices.
- `crates/ss-client-core/src/voice/mod.rs`: `VoiceDeviceState::Routes`,
  `DeviceRoute` re-export, `mod host`.
- `crates/ss-client-core/src/lib.rs`: re-exports `DeviceRoute`.
- `apps/desktop/src-tauri/src/types.rs`, `commands.rs`: the payload variant
  and its mapping.
- `apps/desktop/src/lib/generated/VoiceDevicePayload.ts`,
  `DeviceRoutePayload.ts`: regenerated by ts-rs.
- `apps/desktop/src/features/voice/{api.ts,api.test.ts,store.ts,VoiceDock.tsx}`
  and one branch in `apps/desktop/src/lib/events.ts`.
- `docs/design/voice.md`: the policy, under Devices.

### Deviations
- The worker's `Devices` no longer has `cfg` gates. VoiceProcessingIO sits
  behind `host::DuplexStream`, so the mock covers it on every platform. Only
  the real `Host` impl and the `DuplexStream` impl for
  `VoiceProcessingStream` are gated.
- `DeviceEvent::requires_reopen` is no longer used by the worker or the
  example. It stays public; `Recovery::observe` replaces it.

### Manual check (needs hardware; not done here)
macOS, USB headset, desktop app. Run each case on the portable backend, then
again with Apple voice processing on. Have a second client in the channel
who plays music or talks continuously.
1. Select the headset as input and output in audio settings and Apply. Join
   voice and check that both directions work.
2. Unplug the headset. Within about a second the dock shows "… unavailable;
   using the system default." for both. You keep hearing the other client on
   the built-in speakers, and they hear you on the built-in microphone.
   There is one dock status line, and no repeating error notice.
3. Wait 10 s, then plug the headset back in. Within about a second audio
   moves back to the headset and the status line goes away. You never left
   the channel.
4. Set both devices to System default with the headset as the macOS
   default. Unplug and replug: the stream follows the default both ways.
5. Only if a Mac without a built-in microphone (Mac mini, Mac Studio) is at
   hand: make the headset the only input and unplug it. The line shows
   "Microphone disconnected. Reconnecting…". Replug: it recovers within
   about a second. Otherwise only the mock tests cover this case.
6. Unplug, then click Apply in audio settings without changing anything
   after replugging. The stream reopens at once.
7. Apple voice processing: unplug and replug the headset. It reopens on the
   new default each time, and the dock clears.
8. Open Activity Monitor for the app during a 5-minute unplug. Memory stays
   flat.

Record the results here. The Windows and Linux rows stay open for T-011
and T-012.
