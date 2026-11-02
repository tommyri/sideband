# T-075 — Installable test builds and build identity

Status: in-progress
Phase: 3
Size: M
Depends on: T-042
Paths: `apps/desktop/`, `.github/workflows/test-desktop.yml`, `docs/testing-app.md`

## Goal
Provide reproducible packages testers can install on a Windows gaming PC,
Mac and Linux laptop, starting with the current scaffold and incorporating
subsequent UI increments. This is the first testing-app milestone task.

## Scope
- Local build command and manually dispatched CI matrix: Windows x64 NSIS
  installer, macOS arm64 app/DMG, Linux x64 deb/AppImage. Document any runtime
  requirements; do not silently claim portable Linux bundles without checking.
- Clearly identify test channel, app version, full git commit, target/arch
  and dirty/local provenance in the app and alongside downloadable artifacts.
  Produce checksums; never embed secrets or machine-specific absolute paths.
- Supply valid platform icons, app metadata, macOS microphone usage string
  and audio-input entitlement. Use stable app identity across test builds.
- Test packages work without production signing secrets. Document Windows
  SmartScreen/macOS Gatekeeper caveats and ad-hoc TCC grant limitations.
  Do not disable OS protections or reset TCC.
- CI uploads workflow artifacts, not public releases. With no configured
  GitHub remote, verify local builds and document the exact hosted-run step.
- Installation, uninstall, permissions, server-address prerequisites and
  version reporting instructions for each target. Clearly state which voice
  and testing UI pieces are still pending.

## Out of scope
Production signing/notarization, updater feeds, public release publishing
(T-057); onboarding/voice UI (T-043/T-044); guided evidence export (T-076).

## Acceptance criteria
- Local package build exercised on the development host.
- Workflow and scripts validate, and each hosted matrix job produces packages
  plus build identity/checksums when a remote is available.
- Installed app exposes its build identity without a terminal.
- Manual install/launch results recorded per OS; missing remote/hardware
  acceptance is explicit, not inferred from build success.

## Notes from implementation
- Added `node scripts/build-test-desktop.mjs` / `just build-test-desktop`
  and a manually dispatched Windows x64, macOS arm64 and Linux x64 workflow.
  It uploads workflow artifacts only; it never publishes releases.
- Packages include a build-info JSON, installation instructions and SHA-256
  checksums. The same identity is compiled into the native Build → About
  this test build dialog and tray menu: version, channel, full commit,
  target, profile, source origin and dirty-tree marker. Ordinary developer
  builds explicitly report unknown commit provenance.
- Added desktop ICO/ICNS variants using the Tauri icon converter and the
  existing icon, macOS 13 minimum, microphone usage text, audio-input
  entitlement, ad-hoc test signing, and explicit Linux PipeWire/ALSA package
  dependencies. No production signing/notarization credentials are needed.
- Local Apple Silicon validation: debug app ZIP and DMG generated successfully;
  all artifact checksums passed. The app bundle passed strict codesign
  verification, contains the microphone text and audio-input entitlement,
  and links only system frameworks/libraries. Launched the bundle and
  verified the exact build identity in its native About dialog.
- Frontend type/lint/format/component checks, packaging-script provenance/
  argument tests and dependency graph checks passed. Workflow YAML parsed.
- Full macOS workspace `just check` passed (formatting, strict Clippy and
  automated tests); hardware/manual tests retain their explicit ignores.
  The generated DMG also passed `hdiutil verify`.
- Remaining acceptance: execute hosted release-profile matrix jobs once a
  GitHub remote is configured, then record fresh Windows/Linux installation
  and macOS downloaded-artifact/Gatekeeper results. Linux test packages use
  Ubuntu 24.04 as their runtime baseline; older-distribution portability and
  AppImage runtime dependencies require validation, not an assumed pass.
- The local artifact is a debug testing scaffold, not performance evidence
  or a completed voice-testing app. T-043 is the next implementation priority.
  See `docs/testing-app.md` for building, installation and current limitations.
