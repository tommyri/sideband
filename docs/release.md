# Packaging and signed updates

All GitHub Actions remain manually triggered. No push, pull-request, tag, or
schedule starts a build. [Release desktop installers](https://github.com/tommyri/sideband/actions/workflows/release-desktop.yml)
calls the reusable packaging workflow only after **Run workflow**.

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
| Secret `TAURI_SIGNING_PRIVATE_KEY` | Complete contents of `sideband.key` |
| Secret `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Key password; empty only for an unencrypted key |

Keep this keypair stable. Changing the public key prevents already-installed
clients from trusting new updates; key rotation needs an explicit migration or
manual reinstall. Missing keys fail update-enabled packaging; they never produce
an unsigned automatic update. Select **Create signed auto-update packages = false**
for the existing manual test-installer path.

The public key and HTTPS endpoint are compiled into the native app. The webview
cannot choose an endpoint, key, URL, version, or package to install. Tauri verifies
the downloaded signature and its signed version before installation. No update is
installed automatically: the app checks on startup/every six hours and exposes
release notes and **Install and restart**. Active calls/audio tests must end first.
A failed download/signature check can be retried. Existing 0.0.1 builds without the
updater need one manual installation of an update-enabled build.

## Cut a development release

Select a version, OSes, optional Intel Mac runner, and whether to enable signed
updates. Windows produces NSIS `.exe` and WiX `.msi`; macOS produces `.dmg`, app
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

## Optional operating-system signing

### macOS

Without credentials, builds are ad-hoc signed and not notarized. Gatekeeper and
Input Monitoring persistence across updates are **not** accepted in this mode.
For Developer ID distribution configure these secrets:

- `APPLE_CERTIFICATE`: base64-encoded exported Developer ID Application `.p12`.
- `APPLE_CERTIFICATE_PASSWORD`: password for that `.p12`.
- `APPLE_SIGNING_IDENTITY`: full Developer ID Application identity.
- `APPLE_ID`, `APPLE_PASSWORD` (app-specific password), `APPLE_TEAM_ID`: notarization credentials.

The manual workflow imports the certificate into a temporary runner keychain.
Tauri signs with the selected identity and notarizes when credentials are present.
Keep the bundle identifier `com.socialspeak.desktop` and signing team stable to
preserve macOS permission identity. The existing hardened-runtime audio-input
entitlement and microphone usage description are retained.

Validate an installed release using `codesign --verify --deep --strict`,
`spctl --assess --type execute`, and `xcrun stapler validate` on the app bundle.
Verify Input Monitoring permissions survive N→N+1, including background PTT.

### Windows

Optional secrets `WINDOWS_CERTIFICATE` (base64 PFX) and
`WINDOWS_CERTIFICATE_PASSWORD` import a signing certificate into the runner's
current-user certificate store. Packaging passes its thumbprint to Tauri and
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
