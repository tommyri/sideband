# T-110 — CI, fuzzing and release follow-ups

Status: awaiting-acceptance
Phase: 4
Size: S
Depends on: T-097, T-098
Paths: `.github/`, `fuzz/`, `deny.toml`, `Cargo.toml`

## Goal
What T-097, T-098 and T-101 left between them is owned by one task.

Source: T-099, checked against `0d0cb16`.

## Scope
- `fuzz.yml` still runs on `ubuntu-latest`; every other workflow pins
  `ubuntu-24.04`. T-098 left it to T-101, which did not change it.
- `fuzz/Cargo.lock` is stale against the workspace (T-101); refresh it and
  make `fuzz.yml` use `--locked`.
- `cargo deny check` fails on RUSTSEC-2026-0459 until `sqlx` allows
  `libsqlite3-sys` 0.39 (T-098). Upgrade when it does, or add a dated,
  scoped ignore with the reason, so `ci.yml` can pass.
- Windows signing still runs inside `tauri build`, where a build script can
  sign with the key (T-097). Build unsigned, then sign and rebundle in a
  separate job, or use a remote signing service. Until then
  `docs/security.md` §11 says so.
- BuildKit and `dtolnay/rust-toolchain` are not pinned by Dependabot
  (T-097).

## Out of scope
Push triggers: CI stays manual-only (maintainer decision, T-098).

## Acceptance criteria
- A dispatched `ci.yml` passes `cargo deny`, and `fuzz.yml` runs every
  target on `ubuntu-24.04` with `--locked`.
- The Windows release job that holds the signing key runs no dependency
  build scripts, or the remaining exposure is recorded in
  `docs/security.md`.

## Notes from implementation

Implemented 2026-11-11 on `task/T-110-ci-release-followups` (from `3eb3b9b`).
Nothing was pushed or dispatched, and nothing on GitHub was changed. The
only GitHub and crates.io calls were reads.

### Fuzzing

- `fuzz.yml` runs on `ubuntu-24.04` and uses the same
  `dtolnay/rust-toolchain` commit as the other workflows (`toolchain:
  nightly` was already an input, so the `nightly` branch commit was not
  needed).
- cargo-fuzz 0.13.2, the newest release, has no `--locked` flag
  (`cargo fuzz build --locked` is rejected as an unknown argument). The
  workflow therefore runs `cargo +nightly fetch --locked --manifest-path
  fuzz/Cargo.toml` before fuzzing, and `git diff --exit-code --
  fuzz/Cargo.lock` after it. With the old lockfile the locked fetch fails
  ("cannot update the lock file ... because --locked was passed").
- `fuzz/Cargo.lock` was refreshed from the workspace lockfile: `cp Cargo.lock
  fuzz/`, then `cargo +nightly update --workspace` in `fuzz/`, which only
  pruned and added `socialspeak-fuzz`, `libfuzzer-sys` 0.4.13 and
  `arbitrary` 1.5.0 (both the versions the old fuzz lockfile had). Every
  other one of the 456 registry packages now has exactly the version the
  workspace `Cargo.lock` pins. A plain `cargo update --workspace` on the old
  file instead left 24 crates on versions that differ from the workspace.
  `fuzz/README.md` has the recipe; it also listed five targets and said
  "each night", both corrected.
- Here (macOS arm64, nightly 2026-10-23, cargo-fuzz 0.13.2): the locked
  fetch passes, `cargo +nightly fuzz build` builds all seven targets
  (3 min 26 s) and leaves `fuzz/Cargo.lock` byte-identical.

### RUSTSEC-2026-0459

- No sqlx release allows `libsqlite3-sys` 0.39. On crates.io (2026-11-11)
  the newest `sqlx` and `sqlx-sqlite` are 0.9.0 (2026-05-21), and
  `sqlx-sqlite` 0.9.0 requires `libsqlite3-sys >=0.30.1, <0.38.0`.
  `libsqlite3-sys` 0.39.0, 0.39.1 and 0.40.0 exist. So `deny.toml` ignores
  the advisory by id, with the reason, the date and a review date of
  2027-02-11. No `Cargo.toml` or `Cargo.lock` change.
- What it affects, from the advisory text and this tree: SQLite before
  3.54.0 can return uninitialized page-cache memory when it opens a
  database with a `-wal` of a different page size. The files must be
  crafted, the vector is local, and the victim must open them.
  `libsqlite3-sys` 0.37.0 here is built `bundled` (through sqlx's `sqlite`
  feature) and bundles SQLite 3.51.3 (`sqlite3.h`). Only `ss-server` links
  it: `cargo tree -i libsqlite3-sys` is empty for `socialspeak-desktop` and
  `ss-cli` without dev-dependencies, and `ss-client-core` has none. The
  server opens only `socialspeak.db` in its own data directory (WAL mode).
  Reaching the bug takes write access to that directory, which already
  allows replacing the database. `ss-server backup` writes a serialized
  image with no WAL, and both restore commands delete the WAL files before
  `tar -xf`. A tar from an untrusted source could still carry a `-wal`.
  Recorded in `docs/security.md` §11.
- `cargo deny --all-features check` and `cargo deny check` (cargo-deny
  0.20.2) print `advisories ok, bans ok, licenses ok, sources ok`. Before
  the ignore, advisories failed on this advisory alone.

### Windows signing

Decision: a separate signing job, not a remote signing service. A remote
service called from the build job is still a signing service that every
build script there can call. It removes the key file from the runner, but
not the exposure this task is about.

Not implemented. The Tauri docs show `tauri build --no-bundle` followed by
`tauri bundle` only for the macOS App Store and the Microsoft Store. They do
not say what `tauri bundle` signs on Windows. The tauri-bundler 2.12.4
source shows that it signs the main binary (after binary patching), the
NSIS plugin DLLs, the uninstaller (through NSIS `!uninstfinalize`, which
runs while the installer compiles), both installers and the WiX output, and
that the CLI runs only `cargo metadata --no-deps`. So the split looks
workable, but only from reading the source, not from the docs. It also
needs changes outside this task's Paths (`scripts/build-test-desktop.mjs`
for a no-bundle Windows build, a bundle-and-manifest step and its tests),
and no Windows build can run here. Signing only the finished installers
afterwards does not work: the uninstaller and the executable inside them
would stay unsigned.

The remaining exposure, now stated in `docs/security.md` §11 and
`docs/release.md` (release gates and Windows):

- The certificate is in the runner user's store for the rest of the Windows
  build job. That includes `pnpm install` with its install scripts, which
  earlier text left out, as well as the desktop tests and `tauri build`.
- Any of that code can sign any file. The runner user is an administrator
  and non-exportable is a crypto API flag, so it may also be able to export
  the key. Earlier text said only that it could sign.

The workflow comment in `test-desktop.yml` says the same.

**Follow-up (needs a task):** in the Windows build, use `tauri build
--no-bundle` and no certificate, and upload the unsigned executable with its
checksum. Then add a `windows-signing` job (fresh `windows-2025`,
`desktop-signing` environment, ref and environment checks). Before any
secret, it downloads and checks the executable and installs the locked Tauri
CLI with `pnpm install --frozen-lockfile --ignore-scripts`. Then it imports
the PFX and runs `tauri bundle --bundles nsis,msi` with the thumbprint
configuration, writes `build-info.json` and `SHA256SUMS`, and replaces the
platform artifact, as `macos-signing` does. Check the result with
`Get-AuthenticodeSignature` on the installed app, the uninstaller and both
installers.

### Dependabot

- BuildKit: the image moved from the `run:` step to
  `.github/buildkit/Dockerfile`, which has one `FROM
  moby/buildkit:v0.34.2@sha256:…` line and is never built. The publish
  step reads that line and refuses it unless it matches
  `moby/buildkit:vX.Y.Z@sha256:<64 hex>`; two `FROM` lines fail. A new
  `docker` entry for `/.github/buildkit` in `dependabot.yml` updates the tag
  and digest. The `github-actions` ecosystem could not do this, since it
  does not support `docker://` references.
- `dtolnay/rust-toolchain`: every workflow now pins `683b588`, which is the
  action's `v1` tag and the head of `master` on 2026-11-11. The comment
  says `# v1` (it said `# master`, and fuzz's `2c76c03` was the `nightly`
  branch, which has no tag). The GitHub docs say Dependabot updates a
  tagged commit to newer tags, and an untagged commit to the newest commit
  on the default branch. `v1` moves with `master`, so after it moves the
  old pin is untagged and Dependabot should propose `master`. This is
  unverified until Dependabot runs.
- Dependabot cannot track: the Rust version (`rust-toolchain.toml` and the
  `toolchain: "1.99.1"` inputs; its `rust-toolchain` ecosystem would update
  only the file and leave the inputs out of line, so it is not enabled), the
  fuzzing nightly, the `cargo-deny`/`cargo-fuzz` versions
  `taiki-e/install-action` installs (newest at run time), runner labels,
  and the NSIS/WiX tools Tauri downloads. Recorded in
  `docs/dependency-maintenance.md` and `docs/security.md` §11.

### Validation here

- `cargo +nightly fetch --locked --manifest-path fuzz/Cargo.toml` and
  `cargo +nightly fuzz build` (seven targets): pass, lockfile unchanged.
- `cargo deny --all-features check`: pass.
- `just check` (nightly rustfmt, clippy `-D warnings`, the workspace tests
  with 57 test binaries passing, `just plan-check`): pass on the first run, with
  no flaky retry.
- `node --test scripts/*.test.mjs`: 40 of 40 pass.
- Workflows: no `actionlint` here. The workflows were parsed with js-yaml and
  checked for `workflow_dispatch`/`workflow_call` triggers only, SHA-pinned
  actions, `timeout-minutes`, existing `needs`, and no `*-latest` runner.
  `dependabot.yml` was parsed too. The changed publish step passes
  `bash -n`, and the BuildKit check was run on the real file and on a
  two-line file. **Unvalidated** until a real run: the workflows
  themselves, the fuzz run on Linux, and Dependabot picking up the new
  directory.

### Files outside the Paths

`docs/release.md`, `docs/security.md` (§11 only), `docs/dependency-maintenance.md`,
this task file, `INDEX.md` and `ACCEPTANCE.md`. `fuzz/README.md` is inside
`fuzz/`. `docs/security.md` §12 still lists the two T-110 gaps. The
`cargo deny` line there is now stale, and the Windows line is still accurate.
§12 was left alone because sibling tasks edit neighbouring lines.

### Tommy's checklist

- [ ] Dispatch `ci.yml` on this branch (or on `main` after the merge).
      `cargo deny` must pass; it fails only if a new advisory has appeared.
- [ ] Dispatch `fuzz.yml`. Each of the seven jobs must run on
      `ubuntu-24.04`, pass **Locked dependencies** and **Lockfile
      unchanged**, and fuzz for ten minutes. A crash is a fuzz finding, not
      a T-110 failure.
- [ ] After the merge, check *Insights → Dependency graph → Dependabot* for
      a `/.github/buildkit` docker entry without errors.
- [ ] On the next server release, the publish log should show `docker
      buildx create` with the image from `.github/buildkit/Dockerfile`.
- [ ] Decide whether the Windows signing split above becomes its own task.
      Until it does, do not configure `WINDOWS_CERTIFICATE`, or accept the
      exposure recorded in `docs/security.md` §11.
- [ ] By 2027-02-11, or as soon as sqlx allows `libsqlite3-sys` 0.39, upgrade
      sqlx and remove the RUSTSEC-2026-0459 ignore.
- [ ] Remove the stale `cargo deny` line from `docs/security.md` §12 when
      merging.

