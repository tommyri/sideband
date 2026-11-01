# macOS audio path (T-013)

Measured on 2026-11-01. MacBook Pro, Apple Silicon, 14 cores, macOS 27.0.1
(26A434). `cpal` 0.18.2 on CoreAudio. Three other workspaces were compiling
during every run, so the load average was 8 to 12. No xrun was reported in
any run.

## Method

- A scratch probe (not committed) opened the default input and output
  through `cpal` directly with `BufferSize::Default` and with
  `BufferSize::Fixed(128 | 240 | 256 | 480)` at 48 kHz. It recorded frames
  per callback, `cpal`'s timestamps, and the wall clock
  (`CLOCK_UPTIME_RAW`, the base CoreAudio host times use) at each callback.
- A Swift probe read the CoreAudio properties `cpal` uses
  (`kAudioDevicePropertyBufferFrameSize`, `Latency`, `SafetyOffset`) plus
  `kAudioStreamPropertyLatency`, which `cpal` does not read.
- The acoustic round trip could not be measured. The system output was
  muted, and changing that is a system setting. The probe plays a 5 ms
  2 kHz burst every 473 ms and correlates it in the capture. It found every
  burst through the "Microsoft Teams Audio" virtual loopback device, which
  checked the method and the timestamp reading below.
- `cargo run -p ss-audio --example loopback` ran on the built-in devices.
  It also ran as an app bundle without `NSMicrophoneUsageDescription`. That
  bundle is the denied-permission case below.

## Built-in devices

CoreAudio reports these fixed latencies in frames at 48 kHz:

| Device | Device latency | Safety offset | Stream latency |
|---|---|---|---|
| MacBook Pro Microphone | 14 | 36 | 2,399 (50 ms) |
| MacBook Pro Speakers | 60 | 48 | 690 (14.4 ms) |
| iPhone Continuity microphone | 6,000 | 100 | 0 |

Apple's header says a stream's total latency is the device latency plus
the stream latency. Every request was granted exactly, on both devices.
CoreAudio's range is 15 to 4,096 frames.

| Request | Granted (frames) | `cpal` output delay | `cpal` input delay | Output path, reported | Input path, reported | Round trip, reported |
|---|---|---|---|---|---|---|
| Default | 512 | 12.9 ms | 11.7 ms | 27.3 ms | 61.7 ms | 89.0 ms |
| 128 | 128 | 4.9 ms | 3.7 ms | 19.3 ms | 53.7 ms | 73.0 ms |
| 240 (ss-audio's request) | 240 | 7.3 ms | 6.0 ms | 21.6 ms | 56.0 ms | 77.6 ms |
| 256 | 256 | 7.6 ms | 6.4 ms | 22.0 ms | 56.4 ms | 78.3 ms |
| 480 | 480 | 12.3 ms | 11.0 ms | 26.6 ms | 61.0 ms | 87.6 ms |

"Reported" counts from the callback to the converter or from the converter
to the callback. Output is buffer + safety + device latency + stream
latency. Input is the same with its own figures. The round trip is the sum,
2 × buffer + 3,247 frames. These numbers come from CoreAudio's properties,
not from sound in the air (see the manual checks).

`ss-audio` asks for the smallest size of at least 5 ms (240 frames).
CoreAudio grants 240 even though it is not a power of two. Going to 128
saves 2.3 ms each way. 30 s runs at 128 and 240 under the build load showed
no xrun. The built-in microphone's 50 ms stream latency is the largest item
in the round trip, larger than any buffer choice. **Decision:** keep the
5 ms request. A smaller one gains little on this hardware.

### What the `cpal` timestamps mean on macOS

`cpal`'s `callback` field is CoreAudio's IO timestamp for the buffer, not
the moment the callback runs. On output it is one buffer plus the safety
offset ahead of the wall clock (for example 6.33 ms at 256 frames). On
input it is one buffer plus the safety offset behind (6.12 ms). `cpal` then
adds buffer + device latency + safety offset to get `playback`, and
subtracts the same from `callback` to get `capture`. It leaves out
`kAudioStreamPropertyLatency`. The Teams loopback showed a write-to-read
time of exactly one buffer (5.33 ms at 256), which matches this reading.

So on the built-in devices `CaptureStream::capture_delay` and
`PlaybackStream::playback_delay` count the buffer twice. They also miss
about 64 ms of stream latency (50 ms in, 14 ms out). The loopback example
prints 13 to 16 ms of capture delay and 9 to 13 ms of playback delay at
240 frames. The reported path is about 56 ms and 22 ms. The T-017
mouth-to-ear figure adds these delays, so on a MacBook it reads low by
about 50 ms. This is recorded in the backlog, not fixed here. A fix should
read the stream latency at open and should be checked acoustically first.

## 44.1 kHz devices and the resampler

- The built-in devices list 44.1, 48, 88.2 and 96 kHz as separate fixed
  ranges. `choose_format` picks 48 kHz whenever a device offers it. `cpal`
  then sets the device's nominal rate to 48 kHz
  (`kAudioDevicePropertyNominalSampleRate`). That change is system-wide:
  other apps on that device are resampled by the HAL, and the rate stays at
  48 kHz after we close. A device left at 44.1 kHz in Audio MIDI Setup
  therefore runs at 48 kHz and needs no resampler.
- Rubato runs only when a device cannot run at 48 kHz at all: a 44.1 kHz-only
  USB interface, or a Bluetooth headset microphone at 16 or 24 kHz. No such
  device was connected here. The resampling path was checked with synthetic
  input.
- **Defect fixed.** `RateConverter` cleared its output buffer only when a
  pull emptied it completely. The capture worker pulls whole 10 ms frames,
  so a partial frame almost always remained, and the buffer grew by every
  sample the stream produced. At 44.1 kHz mono that was 192 KB/s: 65 MB of
  resident memory after five simulated minutes, about 1.4 GB in a two-hour
  call. The buffer is now shifted down once 8,192 consumed samples
  accumulate. The test
  `resample::tests::pulling_whole_frames_keeps_the_output_buffer_bounded`
  covers this. It fails on the old code with 960,000 pending samples.
- Added latency on a resampled stream, from impulse runs: filter delay
  2.9 ms at 44.1 → 48 kHz, 2.7 ms at 48 → 44.1 kHz, 5.3 ms at 24 → 48 kHz,
  8.0 ms at 16 → 48 kHz. The fixed 480-frame input chunk also holds samples
  for up to 10.9 ms at 44.1 kHz before anything comes out.

## Microphone permission

- Before the first run, `AVCaptureDevice.authorizationStatus` and
  `AVAudioApplication.recordPermission` both read *not determined* for the
  Claude Code host process. The first capture open from that process took
  1.95 s. Afterwards both read *granted*. It is not known whether a prompt
  appeared and was answered. A later open from the same process took 41 ms.
  The bundle without a usage description (next point) opened in 16 ms.
- **A denied microphone does not fail in CoreAudio.** An app bundle without
  `NSMicrophoneUsageDescription` gets no prompt. Its build and play calls
  succeed and the callback runs at the normal rate (93 per second), but
  every sample is exactly 0.0. `cpal` maps only `kAudioUnitErr_Unauthorized`
  to `ErrorKind::PermissionDenied`, and that error did not occur. For such a
  bundle the stored status stays *not determined*.
- **Missing behaviour, added:**
  - `device::Error::PermissionDenied`.
  - `device::microphone_access()` reads `AVAudioApplication.recordPermission`
    on macOS 14 and newer. It never prompts. On macOS 13 and on other
    platforms it returns `Unknown`.
  - `DeviceManager::open_capture` returns `PermissionDenied` when the stored
    decision is *denied*, checked before the open and again after it (the
    first-use prompt can be refused during the open). `cpal`'s
    `PermissionDenied` kind maps to the same variant.
  - The callback notes the first sample that is not digital zero
    (`Counters::has_signal`). If a capture stream has callbacks but no such
    sample one second after it opened, the device poll emits
    `DeviceEvent::SilentInput { id, access }` once. A real microphone always
    has a noise floor. The loopback example, run from the bundle without a
    usage description, printed
    `SilentInput { id: "coreaudio:BuiltInMicrophoneDevice", access: Undetermined }`
    after about one second. Run from the granted host, it printed nothing.
- The stored *denied* path could not be exercised. Creating that state
  needs a person to refuse a prompt, or a change to privacy settings.

### Tauri bundle (note for T-057)

`Info.plist` must contain `NSMicrophoneUsageDescription`. Without it macOS
never asks, the status stays *not determined*, and the microphone delivers
zeros with no error. `SilentInput` with `access: Undetermined` is the
symptom. A signed hardened-runtime build also needs the
`com.apple.security.device.audio-input` entitlement. If the app is ever
sandboxed, it also needs the sandbox's microphone entitlement; check the
current key name at that point. In Tauri 2 the usage description goes in
`src-tauri/Info.plist`, which the CLI merges into the generated one. The
entitlements file is referenced from `bundle.macOS.entitlements` in
`tauri.conf.json`.

## Default device changes

- `DeviceManager` polls every 500 ms and emits `DefaultChanged` when the
  default input or output moves. A device that disappears emits `Lost` for
  an open stream and `Removed`.
- In `cpal` 0.18.2, an output opened on the default uses CoreAudio's
  default-output unit. It follows a new default by itself and calls the
  error callback with `ErrorKind::DeviceChanged`, which ss-audio ignores.
  The stream's `DeviceId` and its chosen format still describe the old
  device. Capture is bound to the device it opened. When AirPods become the
  default input, an open capture stays on the built-in microphone until it
  is reopened.
- **Added:** `DeviceEvent::requires_reopen(direction, open_id,
  follows_default)`. It is true for a `Lost` stream, and for a
  `DefaultChanged` to a different device when the stream was opened on the
  default. The loopback example now drops and reopens a side on either,
  retries every 500 ms while no device is present, and prints each event
  and each reopen. The tests cover an AirPods connect followed by a
  disconnect, replayed from inventory snapshots.
- `ss-cli` reopens only on `Lost`. A CLI call therefore keeps capturing from
  the built-in microphone after AirPods connect. Its output follows by
  itself. T-041 should use `requires_reopen` for both directions.
- Changing the default device by script needs no privileges
  (`AudioObjectSetPropertyData` on `kAudioHardwarePropertyDefaultInputDevice`,
  or `SwitchAudioSource`). It still changes the user's Sound settings, so it
  was not run here. The reopen logic was tested with replayed snapshots,
  and the live check is manual.

## `kAudioDevicePropertyVoiceActivityDetectionEnable` as a future VAD source

Documentation only. Nothing in ss-audio uses it.

- The selectors are `'vAd+'` (enable, settable UInt32) and `'vAdS'`
  (state, read-only UInt32: 1 means voice). They arrived with macOS 14
  (Sonoma). That date comes from Apple's release material and was not
  checked on a macOS 14 to 26 machine here. The macOS 27 SDK header has no
  per-constant availability mark. Use `AudioObjectHasProperty` on the input
  scope at run time, and treat macOS 13 as not supported. On macOS 27 both selectors exist on every input
  device here: built-in, Continuity iPhone, and the Teams virtual device.
  Enable reads 0 by default. macOS 27 adds
  `kAudioDevicePropertySuggestedReferenceDevice` (`'eord'`), the output used
  as the echo reference.
- What it is: an echo-cancelled voice detector run by the system on the
  input. It still works while the process is muted with
  `kAudioDevicePropertyProcessMute`, but not under a hardware mute. Clients
  are told to add a property listener on `'vAdS'` rather than poll. The
  state is 0 whenever input is not running.
- Fit: it gives one bit, on the HAL's listener thread, with undocumented
  timing and no threshold to tune. It cannot replace T-052's gate, which
  needs per-frame decisions, pre-roll, and the same behaviour on all three
  platforms. It does fit a macOS-only "you are muted but talking" hint, the
  same idea as AUVoiceIO's muted-talker detection in T-054. **Decision:** do
  not use it for T-052. Reconsider it for a muted-talker hint once T-054
  exists, and measure the detection delay then.

## Remaining manual checks (maintainer)

These need hardware, a person, or a settings change.

1. **Acoustic round trip on built-in devices.** Unmute the output and set
   the volume to about 50%. Rerun a click-correlation measurement at 128,
   240 and 256 frames. Check that the round trip is close to the reported
   73 to 78 ms, and decide whether the stream-latency correction in the
   backlog goes in.
2. **AirPods connect and disconnect.** Wear headphones or AirPods, because
   the loopback feeds the microphone into the output. Run
   `cargo run -p ss-audio --example loopback -- 180`, then connect AirPods.
   Expect `Added`, then `DefaultChanged` for capture and playback, then
   `reopened capture on coreaudio:<AirPods UID> at 16000 or 24000 Hz` and a
   `LowQualityInput` event, then `reopened playback`. Disconnect them. Expect
   `Lost` and `Removed`, then reopens on the built-in devices. The process
   must not exit, and audio must continue after each reopen. Write down how
   long each reopen took (it is printed) and whether output had a gap.
   Without AirPods, switching the input in Control Center to the Continuity
   iPhone microphone exercises the same path.
3. **USB interface.** Plug one in, make it the default, and run the same
   loopback. Note the granted rate and buffer size (Swift probe or Audio
   MIDI Setup), and whether `resampler_active` is set. If the interface
   supports only 44.1 kHz, the resampler runs. Leave it running five minutes
   and check that resident memory stays flat. Then unplug it while the
   loopback runs.
4. **Denied permission in a real bundle.** In a build with
   `NSMicrophoneUsageDescription` (T-057), click "Don't Allow" at the
   prompt, or turn the app off under Privacy & Security > Microphone. Check
   that opening capture fails with `PermissionDenied`.
5. **Five-minute glitch listen** (carried from T-010) at the 240-frame
   default on headphones.
