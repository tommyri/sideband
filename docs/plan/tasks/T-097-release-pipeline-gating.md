# T-097 — Gate releases: main only, tested commit, isolated signing, provenance

Status: todo
Phase: 4
Size: M
Depends on: T-057, T-034
Paths: `.github/workflows/release-desktop.yml`, `.github/workflows/release-server.yml`, `.github/workflows/test-desktop.yml`, `.github/workflows/ci.yml`, `.github/dependabot.yml`, `scripts/release-desktop.mjs`, `scripts/build-test-desktop.mjs`, `deploy/Dockerfile*`, `docs/release.md`, `docs/publishing.md`

## Goal
Nothing reaches the update feed or the image registry unless it was built
from a reviewed, tested commit on `main`, and the signing key is visible
only to the step that signs.

Source: 2026-11-09 review at `d861a0a`. Read from the workflow files; no
workflow was run. Needs repository admin rights for the environment setup,
so part of this is the maintainer's to do.

## Scope
- No workflow has an `environment:` or a ref check, and
  `release-desktop.yml` passes `secrets: inherit`. A dispatch with the
  branch selector on a task branch publishes a signed build to the
  `desktop-updates` feed for every installed client; the feed refuses to
  move backwards, so recovery needs a higher version. `workflow_dispatch`
  also runs the workflow file from the chosen ref, so any branch with write
  access can read the updater key. Gate the publish jobs on
  `refs/heads/main` or protected tags, and move the signing secrets into a
  GitHub Environment limited to `main` with a required reviewer.
- `release-server.yml` builds and curls two endpoints; nothing ties the
  published commit to green clippy, tests or `cargo deny`. The version
  input is not checked against the tag: dispatching on `server-v0.2.0` with
  the default input overwrites `:0.0.1` and `:latest` with other code, and
  the binary still reports the workspace version. The release job needs
  only `build`, not `image`. Call `ci.yml` as a required job, derive or
  validate the version from the tag, refuse to overwrite an existing
  version tag, and move `latest` forward only. `docs/publishing.md` says
  publishing happens only on a `server-v*` tag; make that true.
- With `updates=true` the Tauri signing key, its password and the Apple
  password are in the environment of the whole `tauri build`, visible to
  every build script and proc macro in the dependency graph. Build without
  them and sign in a separate minimal step (`tauri signer sign`); in
  `prepare`, pass only whether the secret is set.
- Server binaries are published without checksums or attestation; desktop
  checksums come from the same job as the artifacts. Add
  `actions/attest-build-provenance` for binaries, images and installers and
  a `SHA256SUMS` for the server.
- Per-architecture `build-<run>-…` image tags are pushed publicly before
  the two-architecture gate and never deleted, which contradicts
  `deploy/README.md`. Delete them after the manifest is created.
- Base images are pinned by tag, not digest; Dependabot has no `docker`
  entry and no cargo entry for `/fuzz`. Add both and pin digests.

## Out of scope
Turning automatic CI back on for pushes (decision recorded in T-098).
Apple notarization and Windows signing acceptance (T-057).

## Acceptance criteria
- A dispatch from a non-main branch fails before any secret is read; note
  the run that shows it.
- A server release for an existing version fails unless explicitly forced.
- The updater key is not present in the environment of the build step.
- Release assets carry checksums and a verifiable attestation;
  `docs/release.md` explains how to verify them.

## Notes from implementation
