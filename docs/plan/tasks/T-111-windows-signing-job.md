# T-111 — Sign Windows releases in their own job

Status: awaiting-acceptance
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

Implemented 2026-11-12 on `task/T-111-windows-signing-job` (from `408ff03`).
Nothing was pushed or dispatched, and nothing on GitHub was changed. The only
GitHub calls were `gh api` reads of the Tauri source at tag `tauri-cli-v2.12.4`.

### What `tauri bundle` does on Windows (source, not docs)

The Tauri docs (Context7, `tauri-docs` v2) show `tauri build --no-bundle`
followed by `tauri bundle` only for the macOS App Store and the Microsoft
Store, and do not say what is signed. The source of `tauri-cli` 2.12.4, which
bundles `tauri-bundler` 2.10.4 (T-110 called it 2.12.4), says:

- `tauri bundle` runs `build.beforeBundleCommand` if set, `cargo metadata
  --no-deps` (target directory, honouring `CARGO_TARGET_DIR`) and `rustc -vV`
  (host triple). It compiles nothing. It finds the main binary at
  `<target>/release/<name>.exe`, where the name is `mainBinaryName` or the
  Cargo package name: here `socialspeak-desktop.exe` (only `src/main.rs`).
- For each package type it patches `__TAURI_BUNDLE_TYPE_VAR_UNK` in the
  executable, signs it, builds the installer, and then restores the unsigned,
  unpatched copy. So the signed executable exists only inside the installers.
- NSIS: signs a copy of the plugin DLLs, passes the signtool command to
  makensis as `!uninstfinalize`, so makensis signs the uninstaller while it
  compiles the installer, then signs the installer. This happens in the bundle
  step itself, so it works the same from `--no-bundle` output: nothing about
  the uninstaller comes from the build.
- WiX: signs the WiX extension DLLs it uses and the MSI.
- `settings.updater()` is `None` with `createUpdaterArtifacts: false`, so no
  updater key is asked for.

Nothing that should be signed is left unsigned. What the job cannot check by
itself is the uninstaller and the executable inside the NSIS installer: the
uninstaller is written only when the installer runs. Those are in the
checklist below.

### Job layout

- `package` (all platforms) is now in no environment and has no ref or
  environment check: it never needs a secret. For an update-enabled release
  (`inputs.environment != ''` and `inputs.updates`), Windows runs
  `build-test-desktop.mjs --no-bundle`, i.e. `tauri build --no-bundle`, and
  uploads `socialspeak-desktop.exe`, `build-info.json` (identity plus
  `unbundled`) and `SHA256SUMS` as
  `sideband-unbundled-x86_64-pc-windows-msvc-<sha>`. `publish` downloads only
  `sideband-test-*`, so that never becomes a release asset. Without updates,
  or for a standalone `test-desktop.yml` dispatch, Windows bundles in the
  build job as before, unsigned.
- `windows-signing` (fresh `windows-2025`, `desktop-signing`, `pwsh`): ref
  check, checkout, `setup-node` 26.13.0 with no cache, environment check,
  `rust-toolchain` 1.99.1, download, then `prepare-windows` with no secret.
  That checks the upload (exactly three regular files, checksums, the
  run's identity, an unsigned x64 PE32+ with the bundle-type marker), copies
  the executable to a fresh `mkdtemp` target directory, and fetches
  `@tauri-apps/cli` and `@tauri-apps/cli-win32-x64-msvc` at the locked
  version from registry.npmjs.org, checked against the sha512 integrity in
  `pnpm-lock.yaml` before they are read. The script reads those tarballs
  itself (files and directories under `package/` only). Then the checkout and
  `node` hash are recorded, the PFX is imported (exactly one code-signing
  certificate with its key, thumbprint as a step output), and one step
  rechecks the checkout and `node` and runs `bundle-windows`. That rechecks
  the upload, the staged executable and the extracted CLI (byte for byte
  against the kept tarballs), refuses `beforeBundleCommand` and `signCommand`
  in any Tauri config, and runs `tauri bundle --ci --bundles nsis,msi` with
  the build's configuration plus `bundle.windows` (thumbprint, SHA-256,
  DigiCert timestamp). The certificates and keys are then removed
  (`always()`), and with the key gone `signtool verify /pa` and
  `Get-AuthenticodeSignature` (thumbprint) run on both installers and on the
  executable from `msiexec /a`. The installers go up as
  `sideband-test-x86_64-pc-windows-msvc-<sha>`.
- Without `WINDOWS_CERTIFICATE` the job still bundles, unsigned, and skips
  verification.

The acceptance criterion's "no `pnpm install`" was read strictly: the job
installs nothing with a package manager, not even `--ignore-scripts`. That is
why the CLI is fetched by the script.

### Validation here

- `node --test scripts/*.test.mjs`: 53 of 53 pass. New tests:
  - the structural test of `windows-signing`: no step before the import
    references `secrets.`; the step order (ref, checkout, environment check,
    download, `prepare-windows`, checkout record, import, `bundle-windows`,
    removal, signtool, upload); with comments stripped, no `pnpm`, `npm`,
    `yarn`, `npx`, `cargo build|test|run|install|check|clippy`, `tauri build`,
    `rust-cache`, `cache:` or `build-test-desktop.mjs --target`; only this job
    references `secrets.WINDOWS_CERTIFICATE`; `package` has no environment and
    no `secrets.`, and its `UNBUNDLED` condition matches this job's `if:`;
  - the macOS structural test now reads only its own job (it used to read to
    the end of the file);
  - the upload check refuses an extra file, a directory, a missing file, a
    symlink, a changed executable, changed `SHA256SUMS`, an extra listed
    artifact, each wrong identity field, and a signed image; the PE check
    refuses signed, x86, DLL, PE32, non-MZ and marker-less images (crafted
    headers);
  - the lockfile reader on the real `pnpm-lock.yaml`, and a resolution with
    a `tarball:` is refused; the tar reader refuses symlinks, hard links, pax
    `linkpath`, `..`, paths outside `package/`, absolute paths, backslashes,
    `:`, case-folded duplicates and a bad header checksum;
  - the CLI install from fake registry responses: a tarball that is not the
    locked one is refused before extraction; a planted, changed or missing
    file fails the recheck; a package with dependencies or another name is
    refused;
  - `bundleWindows` with a fake `tauri`: it runs `node tauri.js bundle --ci
    --bundles nsis,msi` from the checked CLI, never `build`, with the
    thumbprint config and `CARGO_TARGET_DIR`, and writes both installers with
    both updater payloads; a signing secret in the environment, a bad
    thumbprint, a changed executable, an extra file next to it, a changed CLI
    or a changed upload stops it before it runs anything.
- The CLI entry points' error paths ran here (usage, empty input, a secret
  in the environment, a bad version, `--no-bundle` on macOS).
- Workflows: no `actionlint` and no `pwsh` here. Both workflows were parsed
  with js-yaml (from the main checkout's `node_modules`) and checked for
  existing `needs`, `timeout-minutes`, SHA-pinned actions and no `*-latest`
  runner; the bash steps pass `bash -n`. The PowerShell steps were not
  parsed. **Unvalidated** until a real run: the workflow itself, every
  PowerShell step, the real registry download and tarball format, `tauri
  bundle` from `--no-bundle` output, signtool and `msiexec /a` on the runner.
- `just check` (nightly rustfmt, clippy `-D warnings`, the workspace tests
  with 58 test binaries passing, `just plan-check`): pass on the first run.

### Not changed, noticed

`apps/desktop/src-tauri/src/main.rs` has no `windows_subsystem = "windows"`
attribute, so the Windows release executable is a console-subsystem program
and may open a console window. Outside this task's Paths (T-108's area).

### Files outside the Paths

This task file, `INDEX.md` and `ACCEPTANCE.md`. In `docs/security.md` only the
Windows paragraph of §11 and the Windows line of §12 changed.

### Tommy's checklist

The signing jobs run only from **Release desktop installers** with updates on
(standalone `test-desktop.yml` dispatches get no environment).

- [ ] Put the real code-signing PFX in `desktop-signing` as
      `WINDOWS_CERTIFICATE` (base64) and `WINDOWS_CERTIFICATE_PASSWORD`. A
      self-signed certificate signs, but the in-job `signtool verify /pa`
      fails on its untrusted root.
- [ ] Dispatch on `main` with only Windows:
      `gh workflow run release-desktop.yml --ref main -f version=X.Y.Z -f
      updates=true -f windows=true -f macos=false -f macos_intel=false -f
      linux=false`. Approve `prepare`. `publish` asks for approval again; reject
      it if this run should not publish.
- [ ] `test package (x86_64-pc-windows-msvc)`: the log shows `tauri build`
      with `--no-bundle` and "Unbundled executable for the Windows signing
      job"; the run's artifacts include `sideband-unbundled-…`.
- [ ] `Windows bundling and signing`: every step passes; the log shows no
      `pnpm`, `cargo build` or `tauri build`, "Checked socialspeak-desktop.exe
      … fetched the locked Tauri CLI", Tauri's output naming both
      installers, and `Successfully verified` from
      `signtool verify /pa` for the setup `.exe`, the `.msi` and
      `socialspeak-desktop.exe` from the MSI image.
- [ ] On a Windows machine, download `sideband-test-x86_64-pc-windows-msvc-…`
      (or the published assets), then in a Developer PowerShell:
      `signtool verify /pa /v .\Sideband_X.Y.Z_x64-setup.exe` and
      `signtool verify /pa /v .\Sideband_X.Y.Z_x64_en-US.msi`.
- [ ] Install with the NSIS installer, then
      `signtool verify /pa /v "$env:LOCALAPPDATA\Sideband\socialspeak-desktop.exe"`
      and `signtool verify /pa /v "$env:LOCALAPPDATA\Sideband\uninstall.exe"`.
      Each must report the Sideband certificate and a timestamp. Record the
      run URL and output here, then mark the two T-111 rows in
      `ACCEPTANCE.md`.
