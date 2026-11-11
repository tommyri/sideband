# T-111 — Sign Windows releases in their own job

Status: todo
Phase: 4
Size: M
Depends on: T-110
Paths: `.github/workflows/test-desktop.yml`, `.github/workflows/release-desktop.yml`, `scripts/build-test-desktop.mjs`, `scripts/build-test-desktop.test.mjs`, `docs/release.md`, `docs/security.md`

## Goal
The Windows code-signing certificate is present only in a job that runs no
dependency install or build scripts, as macOS signing already is (T-097).

Source: T-110, which chose this design and left it unbuilt. Its notes and
`docs/release.md` hold the job outline.

## Scope
- Today the certificate is imported at the start of the Windows build job
  and stays in the runner user's store through `pnpm install`, the desktop
  tests and `tauri build`. Any of that code can sign with it, and the runner
  user is an administrator, so assume it can be exported.
- Build with `tauri build --no-bundle` and no certificate, upload the
  output, then bundle and sign on a fresh Windows runner with
  `tauri bundle`. T-110 read tauri-bundler 2.12.4: `tauri bundle` runs only
  `cargo metadata --no-deps` and signs the main executable, the NSIS plugin
  DLLs, the uninstaller and both installers. Signing only the finished
  installers is not enough.
- Check the uploaded build output before the certificate is imported, as
  the macOS signing job does (T-097).
- Update `docs/security.md` §11 and §12 and `docs/release.md`.

## Out of scope
Remote signing services. macOS signing.

## Acceptance criteria
- The job that imports the certificate runs no `pnpm install`, `cargo
  build` or dependency build script; a structural test of the workflow
  asserts it.
- On a dispatched test build, `signtool verify /pa` passes for the
  executable, the uninstaller and both installers.

## Notes from implementation
