# T-118 — Show real capture inputs and open their full channel count

Status: awaiting-acceptance
Phase: 3
Size: S
Depends on: T-088, T-106
Paths: `crates/ss-audio/src/device/`, `crates/ss-client-core/src/voice/`, `apps/desktop/src-tauri/src/audio.rs`, `apps/desktop/src/features/settings/audio/`

## Goal
The microphone input picker offers only the inputs the device has, and the
chosen input is actually the one sent.

Source: T-106. The maintainer chose "fix both" on 2026-11-12 for the two
open questions T-088 left.

## Scope
- The device list reports each capture device's input channel count, and
  the "Microphone input" select (Audio settings › Devices) offers Input 1 to
  that count, plus "Average of all inputs" when there are two or more. A
  saved choice above the count falls back to Input 1, as the stream already
  does.
- `choose_format` prefers a mono config when a host offers one (some ALSA
  `plug` devices), which silently ignores Input 2 or Average. When the
  selection needs more than one channel, prefer the device's full channel
  count.

## Out of scope
Per-channel gain and mixing matrices.

## Acceptance criteria
- Unit test: with a device offering mono and four-channel configs, Input 3
  opens four channels and sends input 3.
- Frontend test: a two-input device offers Input 1, Input 2 and Average
  only.
- Manual: on a multi-input interface the picker lists its real inputs
  (merge with T-088's acceptance row).

## Notes from implementation

Implemented 2026-11-16.

### Findings, checked by tests written first
- Mono for Input 3: confirmed. `stream::tests::input_three_opens_every_channel_and_sends_input_three`
  gives the device one config of 1 channel and one of 4, both at 48 kHz.
  For `CaptureChannel::Index(2)` the old code returned `(1, [1.0, -1.0])`.
  It opened one channel and sent input 1. `rank` put the requested graph
  count first, and the mono config matched it.
- Picker: confirmed. The vitest "offers only the inputs the device has"
  got 17 options for a two-input device, not three. A saved Input 5 on a
  two-input device showed "5". A saved Average on a mono device showed
  Average.

### Counting rule
- A device has as many inputs as the widest of its input configs
  (`select::input_count`). Three hosts list the real layout:
  - CoreAudio lists the device's total in every config;
  - WASAPI shared mode lists the mix format;
  - PipeWire lists the node's channels.

  An ALSA `hw` device lists its real counts, sometimes mono beside the
  full count.
- Some lists offer every count from 1 to `CONVERTING_CHANNELS` (31) or more.
  They come from a plug-in that converts any layout, not from the
  hardware:
  - ALSA `plug`: cpal caps its `u32::MAX` at 64;
  - the `default`, `pulse` and `pipewire` ALSA plug-ins: 32 or 64;
  - cpal's PulseAudio host: 1–31. It is not compiled in.

  Such a device counts as 2, the stereo layout cpal's default config picks
  for it. A list with gaps is the hardware's own (`[1, 2, 32]` is 32).
- No configs (an ALSA device another program holds, or a host error):
  `None`. The picker then offers T-088's Inputs 1–16 and Average.
- The stream uses the same count. `select::capture_channels` asks
  `choose_format` for the full count when mono needs more than input 1:
  another input the device has, or Average on two or more inputs.
  Otherwise it asks for the graph count, as before. Input 1 still prefers
  a mono config. A missing input resolves to input 1, so it opens mono too.
  Stereo still asks for 2.

### What macOS reports
cpal 0.18.2, this MacBook Pro, 2026-11-16. Read with a temporary ignored
test, since deleted. Log: `T-118-macos-input-configs.log` in the session
scratchpad.

| Device | Configs | Count |
|---|---|---|
| MacBook Pro Microphone (built in) | 4, all 1 channel: 44.1, 48, 88.2 and 96 kHz | 1 |
| HD Pro Webcam C920 (USB) | 3, all 2 channels: 16, 24 and 32 kHz, no 48 kHz | 2 |
| iPhone microphone (Continuity) | 1 channel at 48 kHz | 1 |
| Microsoft Teams Audio (virtual) | 1 channel at 48 kHz | 1 |

- No multi-input interface was connected. CoreAudio lists one total per
  config (T-088 read `host/coreaudio/macos/device.rs`), so an interface
  should show its full count. That is the manual check.
- Listing cost, debug build, six devices: 5.0–5.2 ms for the inventory
  alone, and 7.2–7.7 ms with the input configs.

### What changed
- `select.rs`: `input_count` and `capture_channels`. `choose_format` now
  takes the channel count to look for, not the graph's.
- `stream.rs`: `choose_capture` is the pure step from a config list and a
  `GraphConfig` to the format and the `CaptureLayout`, and is the test
  seam. `CaptureStream::open` uses it. Playback is unchanged.
- `watch.rs`: `DeviceInfo::input_channels: Option<u16>`.
- `manager.rs`: `devices()` and `available_devices()` fill it
  (`Listing::WithInputs`). The 500 ms hot-plug poll and `default_output`
  keep the cheap listing (`Listing::Inventory`), because ALSA opens every
  device to list its configs. Hot-plug `Added` events therefore carry
  `None`. The new `DeviceManager::system_default_input()` reads the
  default host's input without listing anything. `default_input()` uses
  it.
- Desktop: ts-rs `AudioDevice` (`inputChannels: number | null`) and
  `AudioEnvironment`, which is now generated and gains `defaultInput`.
  `store.ts` re-exports them instead of its hand-written interface and
  adds `inputCount` and `effectiveInput`.
- "Microphone input" offers Input 1 to the count of the chosen microphone,
  or of the system default input while "System default" is chosen, plus
  "Average of all inputs" from two inputs up. A saved input the device
  lacks:
  - the select shows Input 1, with "This device has no Input 5; it uses
    Input 1." (or "…one input…");
  - the setting keeps it until the user picks another, so it comes back
    with the interface. The stream never rewrites the setting either.

  The select stays disabled with Apple voice processing, and the hint is
  hidden then.
- `docs/design/voice.md`: one paragraph under Devices.

### Tests
- `select.rs`:
  - `a_device_has_as_many_inputs_as_its_widest_config`;
  - `a_converting_plug_in_counts_as_stereo`;
  - `only_a_selection_past_input_one_opens_the_full_count`.
- `stream.rs`: `input_three_opens_every_channel_and_sends_input_three`
  covers Input 3, Average, Input 1 (mono) and Input 8 on four inputs
  (mono, input 1). It is the ALSA `plug` mono case, run through a fake
  config list.
- `src-tauri/src/audio.rs`: `the_device_list_carries_each_input_count`.
- Vitest:
  - "offers only the inputs the device has": two inputs give Input 1,
    Input 2 and Average; eight inputs give 8 and Average; mono gives
    Input 1; an unknown count gives 17 options;
  - "shows input 1 for a saved input the device does not have, and keeps
    it";
  - "falls back from Average on a mono device";
  - the Apple test now also checks that the select is disabled.
- Results:
  - `just check` passes: nightly rustfmt, clippy `-D warnings`, the
    workspace tests and `plan-check`;
  - `cargo clippy -p socialspeak-desktop --all-targets -- -D warnings`
    and `cargo clippy -p ss-audio --features macos-voiceio --all-targets
    -- -D warnings` are clean;
  - `cargo test -p socialspeak-desktop` passes (106);
  - in `apps/desktop`, `tsc -b`, `eslint .`, `prettier --check .` and
    `vitest run` pass (161 tests);
  - the lockfile is unchanged and pnpm was not run.
- Cross-checks stop in C build scripts, without a sysroot:
  - `cargo check --target x86_64-unknown-linux-gnu -p ss-audio
    --all-targets` stops at `alsa-sys` (pkg-config);
  - `--target x86_64-pc-windows-msvc` stops at `opusic-sys`.

### Files outside the Paths
- `apps/desktop/src/lib/generated/AudioDevice.ts` and
  `AudioEnvironment.ts`: new, written by ts-rs.
- `docs/design/voice.md`, `docs/plan/tasks/INDEX.md` and
  `docs/plan/tasks/ACCEPTANCE.md`.

### Deviations
- A converting plug-in counts as two inputs. On ALSA `plug` or `default`,
  a four-input card offers Inputs 1–2. Its `hw:` device offers all four.
- `notifications.rs` plays cues through `available_devices()`, which now
  also reads input configs: about 2.5 ms more here on a debug build with
  four inputs. On ALSA it opens each capture device. The file is outside
  the Paths and is unchanged. If cues lag on Linux, give it an
  output-only listing.

### Manual checks
These need hardware and were not done here. They are merged into T-088's
interface row in `ACCEPTANCE.md`. On macOS, with a two- or eight-input USB
interface:
1. Audio settings › Devices, pick the interface. "Microphone input" lists
   Input 1 up to the interface's count, and "Average of all inputs".
2. Pick the built-in microphone: only Input 1. With "System default", the
   list follows the default input.
3. Save Input 2 on the interface, then pick a mono device. The select shows
   Input 1 with the hint. Apply and Test mic: the log shows
   `opened capture … input=1`. Pick the interface again: Input 2 is back.
4. On Input 2, the log shows `opened capture … channels=<count> input=2`.
5. Optional, on Linux: on an ALSA `hw:` device that lists mono and its
   full count, Input 2 logs `channels=<count>`, not 1.

Record the results here.
