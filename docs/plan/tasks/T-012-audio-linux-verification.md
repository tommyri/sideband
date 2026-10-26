# T-012 — Linux audio path verification (PipeWire native, quantum, fallbacks)

Status: todo
Phase: 1
Size: S
Depends on: T-010
Paths: `crates/ss-audio/src/device/linux.rs` (only if needed), `docs/plan/notes/audio-linux.md`

## Goal
Confirm the Linux path uses PipeWire natively with a sensible quantum and
falls back cleanly to PulseAudio/ALSA.

## Scope
- Verify cpal selects the PipeWire backend when available; log which backend
  is active and surface it in diagnostics.
- Set stream properties: `node.latency = 480/48000` (10 ms) request,
  `media.category = Communication`, `media.role = Communication`, app name/icon
  so it shows correctly in `pavucontrol`/Helvum.
- Measure round-trip latency on (a) PipeWire default config, (b) with the
  requested quantum; document.
- Test the PulseAudio and ALSA fallbacks in a container without PipeWire.
- Flatpak consideration: note what portal permissions would be needed; no
  implementation.

## Out of scope
Packaging.

## Acceptance criteria
- `docs/plan/notes/audio-linux.md` with measurements and decisions.
- On a PipeWire system, the app appears as a Communication stream with the
  requested latency visible in `pw-top`.
- Fallback path opens and plays audio when PipeWire is absent.

## Notes from implementation
