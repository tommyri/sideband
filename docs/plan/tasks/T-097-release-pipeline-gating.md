# T-097 — Gate releases: main only, tested commit, isolated signing, provenance

Status: in-progress
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

No workflow was run, pushed or dispatched, and nothing on GitHub was changed.
Read-only `gh api` and registry calls on 2026-11-10 checked the findings:

| Finding | Result |
|---|---|
| No `environment:`, no ref check, `secrets: inherit` | Confirmed in the files. The repository has no secrets, variables or environments yet, so no key has been exposed. `main` has no branch protection or ruleset. |
| Server version not tied to the tag or the binary | Confirmed. The workspace version is 0.1.0; `:0.0.1` and `:latest` were published from a dispatch on `main` (2026-11-06), so `docs/publishing.md` was wrong. |
| Updater key in the whole `tauri build` | Confirmed. Tauri 2.12.4 also refuses `createUpdaterArtifacts: true` without the private key ("A public key has been found, but no private key"), so updater artifacts had to be turned off in the build, not only the env var removed. |
| No server checksums or attestation | Confirmed. |
| Per-architecture tags left public | Confirmed: `build-42280236293-1-amd64` and `-arm64` are in the GHCR tag list. The suggested fix is wrong: on GHCR a tag is a package version, i.e. the manifest, and the `0.0.1` index points at those manifests, so deleting them breaks the image. The workflow no longer creates them. |
| Base images by tag; no docker or `/fuzz` Dependabot entry | Confirmed. |

What changed:

- `release-desktop.yml`: `prepare` and `publish` run in the `desktop-release`
  environment. Each starts with a `refs/heads/main` check, then
  `scripts/release-gate.mjs environment` checks over the API that the environment
  allows only `main` and requires a reviewer. `prepare` gets only
  `secrets.TAURI_SIGNING_PRIVATE_KEY != ''`. `ci.yml` is a required job. No
  `secrets: inherit`; the builds use `desktop-signing` for OS signing only.
  `publish` installs the locked Tauri CLI with `--ignore-scripts`, signs in one
  step (`release-desktop.mjs sign`, env of `PATH` and the key), verifies every
  signature against the public key and version (`stage`), attests all assets
  with `actions/attest`, then uploads.
- `test-desktop.yml`: optional `environment` input; ref and environment checks
  first when set. The build step has no signing secret, and
  `build-test-desktop.mjs` refuses to start if one is present. Updater payloads
  are listed unsigned; the script writes the macOS `.app.tar.gz` in Tauri's
  layout. macOS is always built ad-hoc. A separate `macos-signing` job (fresh
  `macos-15` runner, `desktop-signing` environment, ref and environment checks)
  is the only place the Apple certificate and notarization key appear. It
  imports the `.p12` before downloading any build output, then
  `build-test-desktop.mjs sign-macos` re-signs the app with the Developer ID,
  rebuilds the ZIP, updater archive and DMG, signs the DMG, and notarizes it with
  an App Store Connect API key from a `0600` file (`--key --key-id --issuer`),
  then staples it. It uses only `/usr/bin` tools and replaces the build artifact,
  so `publish` signs the updater payload for, checksums, attests and uploads the
  signed packages. The app ZIP is the only thing that job unpacks. Before
  extraction, `sign-macos` reads the ZIP directory itself and compares it with
  `zipinfo -1`. Local headers and data descriptors must match the directory,
  only store and deflate are accepted, entries may not overlap, and link targets
  are bounded (4 KiB) and CRC-checked. It refuses absolute paths, `..`, links
  that are absolute or leave the bundle when resolved through the other links
  component by component, entries written through any link, duplicates
  (case-folded), special files, setuid bits, every `__MACOSX` entry, and
  anything outside one top-level `<Name>.app/`. The build now zips with
  `ditto --norsrc --noextattr --noqtn --noacl`, which writes no sidecars;
  `--sequesterRsrc` wrote one for nearly every entry. The signing job extracts
  with the same flags into a fresh `mkdtemp` directory, apart from the
  keychain's own directory and the checkout, then checks that the `.app` is a
  real directory and that every link resolves inside it, and deletes the
  directory either way. Nothing is mounted. Apple ID and app-password notarization was removed rather than
  kept as a fallback: notarytool takes that password only in argv.
- `release-server.yml`: runs only on a `server-vX.Y.Z` tag whose commit is on
  `main` (compare API). The version comes from the tag and must equal the
  `ss-server` crate version. An existing image tag or release fails unless
  `force`; `latest` moves only forward (read from its version label). CI is a
  required job. Builds only smoke-test locally (and check `ss-server --version`);
  `publish` (environment `server-release`) repeats the plan, writes `SHA256SUMS`,
  attests the binaries, pushes one two-platform index with buildx (pinned
  BuildKit), attests it in the registry, and creates or (forced) updates the
  release.
- `ci.yml`: `workflow_call`. `deploy/Dockerfile*`: digests pinned;
  `Dockerfile.release` copies `ss-server-${TARGETARCH}`. `dependabot.yml`:
  `cargo` for `/fuzz`, `docker` for `/deploy`.
- Trust boundary: `build-info.json` and its checksums are written by the
  untrusted build job, so the checks against them in `sign`, `stage` and
  `sign-macos` catch corruption between jobs, not tampering. The signing jobs
  sign whatever the build produced. What they protect is the keys. The trust
  boundary is the build's inputs: a reviewed commit and the locked dependency
  graph.
- Windows: the certificate import moved to before any dependency code in the
  Windows build job. Signing stays inside `tauri build`, because Tauri signs the
  app, the NSIS uninstaller and both installers while bundling. A build script
  there cannot read the PFX or its password but can sign with the key (and the
  runner user is an administrator). Follow-up: build unsigned and sign plus
  rebundle in a separate job, or use a remote signing service.
- New `scripts/release-gate.mjs` and its test (outside the listed Paths), and
  small fixes to `deploy/README.md` and `docs/desktop-releases.md`, which
  described the old server flow.

Validated here: all script tests (`node --test scripts/*.test.mjs`, 31 pass).
Also refused: a `__MACOSX` sidecar addressed through a framework link, a
two-link chain that escapes although each link is inside on its own, a link
loop, local/central mismatches in method, flags, CRC and sizes, a link using a
data descriptor, an unsupported method, a file whose data descriptor disagrees,
and deflated link targets over 4 KiB (declared or actual).
They include crafted ZIPs, built by a small writer in the test, that are refused
before anything is extracted: an absolute path, `../escape`, links to `/` and
`$HOME` followed by a file written through them, a link leaving the bundle, and
a top-level `.app` that is itself a link. They also include post-extraction
checks (outside, dangling, symlinked app, stray files), and a real `ditto` ZIP
with framework links (made with the build's flags, with an extended attribute
set) that passes, has no `__MACOSX` entries and matches `zipinfo -1`.
Not handled yet: nested code. `sign-macos` re-signs only the outer bundle,
which is enough for the current app (one executable, no frameworks or
sidecars). If frameworks or `externalBin` sidecars are added, they must be
signed inside-out with the Developer ID first, or notarization fails.
`sign-macos` ran for real on this Mac with an ad-hoc identity and a compiled
app. The app came out signed with the hardened runtime and the audio-input
entitlement, the DMG holds the app and an Applications link, and the
checksums match.
Signature verification is tested against real `tauri signer` 2.12.4 output; a
local sign-and-stage ran with the real CLI and a disposable key. The gate's
`server-plan` was run against live GHCR and the API for this repository
(0.1.0: proceed, latest moves; 0.0.1: refused, crate is 0.1.0; a branch ref:
refused). `Dockerfile.release` built for both architectures, and the pinned
BuildKit produced a two-platform index locally. No `actionlint` was available:
the workflows were parsed with js-yaml and checked for SHA pins, `needs`,
reusable-workflow inputs and a ref check before any secret, nothing more.
Unverified until a real run: `environment: ''` meaning no environment for
standalone test builds, the environment API with `GITHUB_TOKEN`, attestation
and GHCR push, Developer ID signing, the keychain import and notarization (no real
certificate or API key), the artifact `overwrite`, and the macOS updater archive
installing through the updater.

Not pinned: the BuildKit image is pinned by digest inside a `run:` step, which
Dependabot does not update; `dtolnay/rust-toolchain` stays on its `master`
commit; runner labels and the runner's default Node in `prepare`/`gate`. The
Compose files' image references are not covered (T-098 owns `deploy/` hygiene).

Manual steps (Tommy, repository admin; details in `docs/release.md`, "One-time
GitHub setup"):

- [ ] Create environment `desktop-release`: required reviewer (you), branch
      policy `main` only. Add secrets `TAURI_SIGNING_PRIVATE_KEY` and
      `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` there.
- [ ] Create environment `desktop-signing`: branch policy `main` only, no
      reviewer. When you configure OS signing, its secrets go there:
      `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
      `APPLE_SIGNING_IDENTITY`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER`,
      `APPLE_API_PRIVATE_KEY` (App Store Connect API key, Developer role),
      `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD`. Not `APPLE_ID` or
      `APPLE_PASSWORD`.
- [ ] Create environment `server-release`: required reviewer, tag policy
      `server-v*` only.
- [ ] Keep no release secrets at repository level; keep the variable
      `TAURI_UPDATER_PUBLIC_KEY`.
- [ ] Rulesets: protect `main` (no force push or deletion, require pull
      requests, yourself as bypass if you push merges) and `server-v*` tags.
- [ ] Run the check loop in `docs/release.md` step 6.
- [ ] Dispatch **Release desktop installers** on a branch other than `main`
      and record the failing run URL here (acceptance 1).
- [ ] First server release: tag `server-v0.1.0` on `main` (the workspace
      version), dispatch on the tag, approve, then run the verification commands
      in `docs/release.md`. Dispatch it again without `force` and record that it
      fails (acceptance 2).
- [ ] First update-enabled desktop release: confirm the build step log shows no
      signing variables and the `sign` step signs every payload (acceptance 3);
      verify an asset with `gh attestation verify` (acceptance 4).
- [ ] With Apple credentials configured: check that `macos-signing` reports
      notarization `Accepted`, and that `spctl --assess` and
      `xcrun stapler validate` pass on the downloaded DMG.
- [ ] Leave the old `build-42280236293-1-*` GHCR tags, or delete them only
      together with `0.0.1`.
