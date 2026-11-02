# T-057 — Packaging, code signing, auto-update, release pipeline

Status: todo
Phase: 3
Size: M
Depends on: T-042, T-075
Paths: `apps/desktop/src-tauri/tauri.conf.json`, `.github/workflows/release-desktop.yml`, `docs/release.md`

## Goal
Friends install a normal app and it updates itself.

Reuse T-075's test packaging and build identity. Production distribution and
updater acceptance belong here; they do not block internal test builds.
T-018 must record a go before declaring a stable/public voice release ready,
but implementation of this task may proceed before that approval.

## Scope
- Tauri bundler targets: macOS `.dmg` (universal or arm64+x64), Windows
  `.msi` + NSIS `.exe`, Linux `.AppImage` + `.deb` (+ Flatpak manifest in
  backlog). Icons, product metadata, `NSMicrophoneUsageDescription`,
  `NSInputMonitoringUsageDescription`-style strings where applicable. T-013:
  the hardened-runtime build also needs the
  `com.apple.security.device.audio-input` entitlement, and without the usage
  string CoreAudio delivers silent zeros rather than an error.
- Signing: macOS Developer ID + notarization (or documented ad-hoc path with
  Gatekeeper caveats for now); Windows signing via a cert if available,
  otherwise document SmartScreen behaviour. Stable macOS signing identity is
  required for TCC (T-049).
- `tauri-plugin-updater`: static JSON manifest hosted on GitHub Releases,
  signed with the Tauri updater keypair; "update available" UI with release
  notes; check on start + every 6 h.
- Release workflow: tag `desktop-vX.Y.Z` → build matrix → sign → upload
  assets + `latest.json`. Nightly channel optional.
- Linux: native audio deps bundled or linked dynamically? Decide (PipeWire
  client lib is dynamically loaded; AppImage must include `libpipewire-0.3`
  or dlopen with fallback).
- `docs/release.md`: how to cut a release, secrets needed.

## Out of scope
App stores.

## Acceptance criteria
- Fresh installs on all three OSes from release artifacts; updater upgrades
  from N to N+1 in a test release.
- Signed macOS build passes Gatekeeper; Input Monitoring grant survives an
  update.

## Notes from implementation
