# T-118 — Show real capture inputs and open their full channel count

Status: todo
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
