# T-075 — Installable test builds and build identity

Status: todo
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
