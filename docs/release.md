# Packaging, signed updates and server releases

All GitHub Actions remain manually triggered. No push, pull-request, tag, or
schedule starts a build. [Release desktop installers](https://github.com/tommyri/sideband/actions/workflows/release-desktop.yml)
calls the reusable packaging workflow only after **Run workflow**.

## Release gates

Nothing reaches the desktop update feed or the image registry unless it was
built from a commit on `main` that passed CI in the same run, and a maintainer
approved the publication.

| | Desktop installers and update feed | Server binaries and image |
| --- | --- | --- |
| Dispatch on | `main` only | a `server-vX.Y.Z` tag whose commit is on `main` |
| Version | the `version` input (mutable during development) | from the tag; must equal the `ss-server` crate version |
| Tested | `ci.yml` runs as a required job of the same run | `ci.yml` runs as a required job of the same run |
| Approval | `desktop-release` environment: when the run starts (`prepare`) and before signing and publishing (`publish`) | `server-release` environment, before `publish` |
| Secrets | updater key only in the `sign` step of `publish`; Apple certificate and notarization key only in the `macos-signing` job; Windows certificate in the Windows build job (`desktop-signing`) | none; `GITHUB_TOKEN` only |
| Overwrites | same-version assets are replaced; feeds never move backwards | an existing version tag or release fails unless `force` is set; `latest` only moves forward |
| Provenance | `SHA256SUMS` per platform and a build provenance attestation for every asset | `SHA256SUMS` and attestations for the binaries and the image |

Every job that publishes or reads a release secret starts with a ref check and
then asks the GitHub API whether its environment has the expected policy
(`node scripts/release-gate.mjs environment NAME`). Until the environments below
are set up, those jobs fail before any secret is read.

These checks live in workflow files, and a branch can change its own workflow
files. The boundary is the environment: its secrets are given only to jobs on
the allowed ref, after approval. The checks make the trusted workflow on `main`
fail closed when that configuration is missing or loosened. Two limits remain
and are not fixed by any workflow file:

- Any branch with write access can dispatch a workflow that uses `GITHUB_TOKEN`
  with `contents: write` or `packages: write` to change releases or push to
  GHCR. Restrict who has write access; protect `main` and `server-v*` tags.
- Steps in one job share a runner. A build script or proc macro can leave a
  background process, a changed script or a PATH shim behind for later steps of
  the same job. This is why the updater key and all Apple credentials are used
  only in jobs on fresh runners that run no build output. Windows code signing
  is the exception: Tauri signs the app, the NSIS uninstaller and both
  installers while bundling, so the certificate is imported into the Windows
  build job's user store, before any dependency code runs. A build script there
  cannot read the PFX or its password, but it can sign with the key, and the
  runner user is an administrator. Moving Windows signing out of the build
  (build unsigned, then sign the binary and rebundle, or sign with a remote
  signing service) is a recorded follow-up.

### One-time GitHub setup (repository admin)

Do these in **Settings** of `tommyri/sideband`. None of them can be done from a
workflow.

1. **Environments → New environment `desktop-release`.**
   - Required reviewers: add yourself. Leave *Prevent self-review* off while
     you are the only maintainer.
   - Deployment branches and tags: **Selected branches and tags**, add the
     branch rule `main`. No other rules.
   - Environment secrets: `TAURI_SIGNING_PRIVATE_KEY`,
     `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (see below).
2. **New environment `desktop-signing`.** No reviewer (the run was approved
   at `prepare`). Selected branches and tags: branch `main` only. Environment
   secrets, only when you configure OS signing (see
   [operating-system signing](#optional-operating-system-signing)):
   `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`,
   `APPLE_API_KEY_ID`, `APPLE_API_ISSUER`, `APPLE_API_PRIVATE_KEY`,
   `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD`. Do not add
   `APPLE_ID`/`APPLE_PASSWORD`: notarization no longer accepts them.
3. **New environment `server-release`.** Required reviewer: yourself.
   Selected branches and tags: add a **tag** rule `server-v*`. No secrets.
4. **Repository secrets: none.** Delete any of the names above from
   *Secrets and variables → Actions → Repository secrets*; a repository secret is
   readable by a workflow on any branch. Keep the repository *variable*
   `TAURI_UPDATER_PUBLIC_KEY`; it is public.
5. **Rules → Rulesets.** `main` currently has no protection, so "on `main`"
   only means "pushed by someone with write access".
   - Branch ruleset for `main`: block force pushes and deletion, require a pull
     request. Put yourself in the bypass list if you merge locally and push.
   - Tag ruleset for `server-v*`: restrict creations, updates and deletions,
     with only yourself in the bypass list.
6. Check the result with the same API the workflows use:

   ```sh
   for e in desktop-release desktop-signing server-release; do
     gh api repos/tommyri/sideband/environments/$e \
       --jq '{name, rules: [.protection_rules[].type], policy: .deployment_branch_policy}'
     gh api repos/tommyri/sideband/environments/$e/deployment-branch-policies \
       --jq '[.branch_policies[] | "\(.type):\(.name)"]'
   done
   ```

   `desktop-release` and `server-release` must list `required_reviewers`, and
   each must show exactly one rule: `branch:main`, `branch:main` and
   `tag:server-v*`.
7. Dispatch **Release desktop installers** on any branch other than `main`.
   It must fail in `prepare` (environment branch policy, or the ref check)
   without running any step that reads a secret. Note the run URL in T-097.

The existing `build-<run>-amd64`/`-arm64` tags in
`ghcr.io/tommyri/sideband-server` came from the old workflow. They point at the
same manifests as `0.0.1` and `latest`: deleting those package versions deletes
those images too. Leave them, or delete the whole `0.0.1` index and its two
platform manifests together once nothing pulls `0.0.1`.

## First-time updater setup

Updater signatures are separate from Apple/Windows code signing. Generate the
production key once on a trusted machine, outside the repository:

```sh
pnpm --dir apps/desktop tauri signer generate --write-keys /secure/path/sideband.key
```

Back up the private key and password securely. Never commit them. Configure:

| GitHub setting | Value |
| --- | --- |
| Repository variable `TAURI_UPDATER_PUBLIC_KEY` | Complete contents of `sideband.key.pub` (base64 text, not a path) |
| `desktop-release` environment secret `TAURI_SIGNING_PRIVATE_KEY` | Complete contents of `sideband.key` |
| `desktop-release` environment secret `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Key password; empty only for an unencrypted key |

Keep this keypair stable. Changing the public key prevents already-installed
clients from trusting new updates; key rotation needs an explicit migration or
manual reinstall. Missing keys fail update-enabled packaging; they never produce
an unsigned automatic update. Select **Create signed auto-update packages = false**
for the existing manual test-installer path.

### Where signing happens

The platform builds run `tauri build` with the public key and endpoint only.
`scripts/build-test-desktop.mjs` refuses to start if an updater key, Apple or
Windows certificate, or notarization credential is in its environment, and
Tauri's own updater signing is off (`createUpdaterArtifacts: false`). macOS
builds are always ad-hoc signed there; Developer ID signing happens in
`macos-signing` (see [macOS](#macos)). The installers are the update payloads
on Windows and Linux; on macOS the script writes `Sideband-test-X.Y.Z.app.tar.gz`
in Tauri's layout. `build-info.json` lists the payloads unsigned.

The `publish` job then, on a fresh runner:

1. installs the locked Tauri CLI with `pnpm install --frozen-lockfile --ignore-scripts`,
   so no dependency code runs;
2. **Sign updater payloads** is the only step given `TAURI_SIGNING_PRIVATE_KEY`.
   It runs `tauri signer sign --app-version X.Y.Z` per payload with an
   environment of just `PATH` and the key, then verifies each signature against
   `TAURI_UPDATER_PUBLIC_KEY`;
3. **Verify and name release assets** checks every checksum, checks each
   signature against the public key and the release version again, and writes the
   per-platform `SHA256SUMS` and feeds;
4. attests every asset with `actions/attest`, then uploads.

A private key that does not match the configured public key stops the run before
anything is uploaded.

The public key and HTTPS endpoint are compiled into the native app. The webview
cannot choose an endpoint, key, URL, version, or package to install. Tauri verifies
the downloaded signature and its signed version before installation. No update is
installed automatically: the app checks on startup/every six hours and exposes
release notes and **Install and restart**. Active calls/audio tests must end first.
A failed download/signature check can be retried. Existing 0.0.1 builds without the
updater need one manual installation of an update-enabled build.

## Cut a development release

Dispatch from `main`, then approve twice in the run page's **Review deployments**:
once for `prepare` (starts the builds and CI) and once for `publish` (signs and
uploads, after every build and CI job passed). Select a version, OSes, optional
Intel Mac runner, and whether to enable signed updates. Windows produces NSIS `.exe` and WiX `.msi`; macOS produces `.dmg`, app
ZIP and (for updates) `.app.tar.gz`; Linux produces `.deb` and `.AppImage`.
Apple Silicon and Intel macOS are separate selectable builds. Intel is opt-in to
avoid spending extra runner minutes. No release is declared stable by this workflow.

```sh
gh workflow run release-desktop.yml --ref main \
  -f version=0.0.2 -f updates=true \
  -f windows=true -f macos=true -f macos_intel=false -f linux=true
```

The existing `vX.Y.Z` prerelease and selected-platform downloads can still be
replaced. Unselected platforms are preserved. A client only offers **newer**
versions: replacing 0.0.1 does not update an installed 0.0.1. Reinstall it manually,
or publish 0.0.2. Avoid mutable versions once stable releases begin.

Each OS/architecture has its own static Tauri JSON feed under the prerelease tag
`desktop-updates` (for example `aarch64-apple-darwin.json`). This deliberately avoids
GitHub's `latest` selector, which excludes prereleases. A Windows-only publication
does not advance Mac/Linux versions. Windows feeds distinguish MSI and NSIS.
Older-version publication never moves a platform feed backwards.

Updater payload names include the source commit and content hash. Keep those
assets: installed clients may have cached a previous feed. Regular human-facing
installer names remain replaceable. Feeds upload last, after all referenced
payloads succeed; publication is not transactional, so rerun a failed publication.
Source identity/checksums remain attached to each platform download.

## Release the server

1. Set `[workspace.package] version` in `Cargo.toml` to the new version and
   merge that to `main`. The binary reports this version; the tag must match it.
2. Tag the merged commit and push only the tag:

   ```sh
   git tag server-v0.1.0 <commit-on-main>
   git push origin server-v0.1.0
   ```

3. Dispatch on the tag, not a branch:

   ```sh
   gh workflow run release-server.yml --ref server-v0.1.0
   ```

4. Approve `publish` in the `server-release` environment once CI and both
   architecture builds have passed.

The `gate` job refuses a ref that is not a `server-vX.Y.Z` tag, a tagged commit
not on `main`, a tag that does not match the crate version, and an existing image
tag or GitHub release for that version. To replace an existing version during
development, dispatch again with `-f force=true`. `latest` moves only when the
version is at least the one `latest` already carries (its
`org.opencontainers.image.version` label), even when forced. `publish` repeats
these checks before pushing.

Each architecture is built and smoke-tested on its own native runner; nothing is
pushed from there. `publish` builds the two-platform image from those same
binaries and `deploy/Dockerfile.release` (which only copies files), pushes it as
one index tagged `X.Y.Z`, `sha-<commit>` and possibly `latest`, attests the
index, and creates the GitHub release with both binaries, the systemd unit and
`SHA256SUMS`. No per-architecture tags are created.

## Verify a download

Each desktop platform has `Sideband-X.Y.Z-<platform>-SHA256SUMS`; the server
release has `SHA256SUMS`. Check a download against it, then check that GitHub
Actions built it from `main` (desktop) or the release tag (server):

```sh
shasum -a 256 --check --ignore-missing Sideband-0.1.0-macos-arm64-SHA256SUMS
gh attestation verify Sideband-0.1.0-macos-arm64.dmg --repo tommyri/sideband \
  --signer-workflow tommyri/sideband/.github/workflows/release-desktop.yml \
  --source-ref refs/heads/main

sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify ss-server-x86_64-unknown-linux-musl --repo tommyri/sideband \
  --signer-workflow tommyri/sideband/.github/workflows/release-server.yml \
  --source-ref refs/tags/server-v0.1.0
gh attestation verify oci://ghcr.io/tommyri/sideband-server:0.1.0 --repo tommyri/sideband \
  --signer-workflow tommyri/sideband/.github/workflows/release-server.yml \
  --source-ref refs/tags/server-v0.1.0
```

Without `--signer-workflow` and `--source-ref`, `gh attestation verify` accepts
an attestation from any workflow on any branch of the repository. A checksum
file from the same release only shows the download is intact; the attestation
shows where it was built. Updater payloads are also verified by the app against
the compiled-in public key before installation.

## Optional operating-system signing

### macOS

Without credentials, builds are ad-hoc signed and not notarized. Gatekeeper and
Input Monitoring persistence across updates are **not** accepted in this mode.
For Developer ID distribution configure these secrets:

- `APPLE_CERTIFICATE`: base64-encoded exported Developer ID Application `.p12`.
- `APPLE_CERTIFICATE_PASSWORD`: password for that `.p12`.
- `APPLE_SIGNING_IDENTITY`: full Developer ID Application identity, as
  `security find-identity -v -p codesigning` prints it.
- Notarization, optional but needed for Gatekeeper: an App Store Connect API key
  (*Users and Access → Integrations → App Store Connect API*, role Developer):
  `APPLE_API_KEY_ID` (the key ID), `APPLE_API_ISSUER` (the issuer ID) and
  `APPLE_API_PRIVATE_KEY` (the complete contents of `AuthKey_<id>.p8`).

These are `desktop-signing` environment secrets. They are used only by the
`macos-signing` job of the desktop build workflow, which runs after the platform
builds on a fresh macOS runner and runs nothing the build produced:

1. ref and environment checks, as in the other release jobs;
2. imports the `.p12` into a temporary keychain and checks that it holds
   `APPLE_SIGNING_IDENTITY`. This happens before any build output is
   downloaded; the `.p12` password is on `security import`'s command line, the
   one secret that still is, with nothing from the build on the runner yet;
3. downloads the ad-hoc macOS packages and checks them against `build-info.json`;
4. `build-test-desktop.mjs sign-macos`, calling only `/usr/bin` tools:
   re-signs the app from the ZIP with the Developer ID, hardened runtime and
   `apps/desktop/src-tauri/Entitlements.plist` from the checkout; rebuilds the
   app ZIP, the updater archive and the DMG from that app; signs the DMG;
   writes the API key to a `0600` file in a private temporary directory,
   submits the DMG with `xcrun notarytool submit --key --key-id --issuer --wait`,
   deletes the key, and staples the DMG; rewrites `build-info.json` and
   `SHA256SUMS`;
5. deletes the keychain and replaces the platform's build artifact, so the
   release signs the updater payload, checksums, attests and uploads the signed,
   stapled packages.

Without `APPLE_CERTIFICATE` the job leaves the ad-hoc packages as they are; an
API key without a certificate fails the run. The DMG is created by `hdiutil`
with the app and an Applications link, without the window layout of Tauri's
DMG. The app inside is notarized by the DMG submission, but the app ZIP and
updater archive are not stapled, so their first launch needs Gatekeeper to reach
Apple. This path has run here only with an ad-hoc identity, not with a real
Developer ID or notarization.
Keep the bundle identifier `com.socialspeak.desktop` and signing team stable to
preserve macOS permission identity. The existing hardened-runtime audio-input
entitlement and microphone usage description are retained.

Validate an installed release using `codesign --verify --deep --strict`,
`spctl --assess --type execute` on the app bundle, and `xcrun stapler validate` on the DMG.
Verify Input Monitoring permissions survive N→N+1, including background PTT.

### Windows

Optional `desktop-signing` environment secrets `WINDOWS_CERTIFICATE` (base64 PFX) and
`WINDOWS_CERTIFICATE_PASSWORD` import a signing certificate into the runner's
current-user certificate store, as the first step of the Windows build job after
the release checks. The build itself does not see either secret, but it runs
with the imported key in the store; see [release gates](#release-gates). Packaging passes its thumbprint to Tauri and
uses SHA-256 with a timestamp service. Hardware-token/cloud signing is not wired
by this PFX path. Without a certificate, document/expect SmartScreen warnings;
updater signatures do not establish Windows publisher reputation.

### Linux

Build baseline is Ubuntu 24.04 x64. Debian packages declare PipeWire and ALSA
runtime dependencies and use package-manager/manual upgrades. Automatic updates
are enabled only when running an AppImage (`APPIMAGE` environment present).
The AppImage explicitly includes `libpipewire-0.3.so.0` under `usr/lib` because
runtime-loaded libraries can otherwise escape dependency discovery. The host
still supplies its audio service, session/portal support and compatible system
libraries; validate on the target distro rather than claiming universal portability.

## Acceptance still requiring installed machines

1. Install fresh macOS arm64/Intel, Windows NSIS/MSI and Linux AppImage/deb packages.
2. Using the same keypair, publish test versions N and N+1; check the prompt,
   notes, explicit install/restart, retained servers/settings and new build identity.
3. Try offline checks/downloads, wrong signatures/keys, and interrupted downloads;
   installation must not begin on failed verification.
4. Publish one platform only, rerun the same version, and publish an older version:
   other platforms must remain usable and clients must not downgrade.
5. Validate macOS Gatekeeper/notarization/TCC with the real Developer ID identity,
   Windows publisher/signature behavior, and Linux runtime dependencies.

T-018 remains the voice-quality release gate. T-057 stays in progress until these
installed upgrade and OS-signing acceptance checks have evidence.

References: [Tauri updater](https://v2.tauri.app/plugin/updater/),
[macOS signing](https://v2.tauri.app/distribute/sign/macos/),
[configuration](https://v2.tauri.app/reference/config/).
