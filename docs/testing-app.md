# SocialSpeak testing builds

These are internal test builds, not stable releases. T-018's voice comparison
is still a release gate. Packages now include server onboarding (T-043): add
an address, compare and trust its certificate fingerprint, register with an
invite or log in, and reconnect with a saved session. Connected servers show live channels, members and voice controls (T-044).
Audio settings (T-051) and global shortcuts (T-058) are available; guided reports
(T-076) follow.
The browser-only design preview still contains sample data.

Sessions and server order survive restarting the app. Select a saved server,
choose **Reconnect / log in**, then **Reconnect with saved session** after
the server check. Automatic reconnect handles temporary outages while the
app remains open. **Remove server** forgets its session on this computer;
it does not delete the account on the server. The native app-data directory
contains private JSON session and pin files, not an encrypted keychain.
Passwords are never saved. Do not include `sessions.json` in test reports.
Open **Audio settings** from the server page, channel sidebar or call gear to
choose devices and talking mode, then **Apply settings**. Settings persist and
apply to an active call without leaving it. Defaults use system devices, PTT,
and no processing. Open mic and voice activation can transmit without holding
a button once applied; mute/deafen still close transmission.

Click a voice channel to join. In PTT mode, hold **Hold to talk** with the mouse, or Space/Enter while it is focused, to transmit. Releasing,
losing focus, muting or deafening closes transmission. Use **Leave** to leave
voice or **Disconnect** to close the server connection. Ctrl/Cmd+Shift+M/D toggle
mute/deafen; Ctrl/Cmd+Shift+Up/Down select channels. Interaction settings offer
double-click joining. Member menus expose playback and permitted moderation.
The displayed latency is network RTT, not mouth-to-ear latency.

For global PTT/mute/deafen, open **Audio settings → Global shortcuts**. Capture
separate keys or mouse buttons, then **Save shortcuts**. Capture may request input
permission; no listener starts automatically while saved bindings are empty.
On Wayland, use system-managed shortcuts when the portal owns bindings. The
screen reports capabilities and limitations; Linux evdev fallback requires
explicit opt-in and user-managed device access. Global PTT uses the selected
PTT transmission mode; it does not turn off VAD/open mic.

Password fields and binding capture suspend the listener and immediately release
PTT. The tray mirrors mute/deafen. The last ten semantic input events are shown
locally for troubleshooting, without recording typed text or uploading anything.
After fixing permissions, choose **Retry saved bindings**. The in-app custom
binding remains available when global PTT is not configured. Actual gaming,
permission/revocation and compositor behavior still need the T-058 manual matrix.

Outside a call, **Test mic** provides 10 seconds of local raw microphone playback
with a 1-second delay (use headphones), **Test output** plays a cue, and VAD
auto-calibration listens to 2 seconds of background noise. Tests stop on close
or **Stop test**, and do not send audio to a server. Device switching speed,
permission flows and audible quality still need real-machine validation.

## Build locally

Install the Rust toolchain pinned by rust-toolchain.toml, Node 24, pnpm 11.9,
and the normal Tauri native build prerequisites for your OS. On macOS use
Xcode Command Line Tools; Windows requires Visual Studio C++ build tools and
WebView2. Linux build packages are listed in .github/workflows/test-desktop.yml.

From the repository:

```sh
cd apps/desktop
pnpm install --frozen-lockfile
cd ../..
node scripts/build-test-desktop.mjs
```

Defaults: native macOS app ZIP + DMG, Windows x64 NSIS installer, Linux x64
deb + AppImage. The app ZIP preserves macOS bundle permissions and symlinks.
For a quick local packaging check:

```sh
node scripts/build-test-desktop.mjs --debug --bundles app
```

The quick command is macOS-only. Debug builds are unsuitable for performance
acceptance; report their profile honestly. Optional macOS Intel packages use
`--target x86_64-apple-darwin` after installing that Rust target. Cross-OS
packaging is unsupported; use the CI matrix or the target machine.

Each invocation creates a new folder in target/test-builds containing packages,
build-info.json, INSTALL.md (this document), and SHA256SUMS. The identity
includes version, full commit, target, test channel, profile, local/CI origin,
and whether the source tree had changes, including untracked files. No source
paths or credentials are copied into the identity.

## Download workflow artifacts

After the repository has a GitHub remote and this workflow is pushed to its
default branch, open Actions → Desktop test builds → Run workflow, selecting
the branch to test. Download the artifact for your OS from the completed run.
Packages are workflow artifacts retained for 30 days; this workflow never
publishes a release or uses production signing credentials.

The full matrix has not yet been executed from this repository because no
GitHub remote is configured. A successful local build does not establish
Windows/Linux installation acceptance.

## Install and identify

Extract the downloaded workflow archive first. Check SHA256SUMS before
installation: `shasum -a 256 <file>` on macOS, `sha256sum <file>` on Linux,
or `Get-FileHash <file> -Algorithm SHA256` in PowerShell; compare the digest.
Only install artifacts from a build/source you trust.

| OS | Installation | Permissions and caveats |
|---|---|---|
| Windows x64 | Run the NSIS setup .exe; launch SocialSpeak from Start. | Unsigned test installers may trigger SmartScreen. Follow your machine's approved installation policy. WebView2 is required; the installer may need network access to install it. |
| macOS 13+, Apple Silicon | Open the DMG and copy SocialSpeak to Applications, or expand the app ZIP and move the app there. Quit an older copy first. | Test builds are ad-hoc signed, not notarized. Gatekeeper may require an explicit approval through System Settings → Privacy & Security. Do not disable Gatekeeper or reset TCC. |
| Linux x64, Ubuntu 24.04 or compatible | Prefer `sudo apt install ./<package>.deb`. Alternatively mark the AppImage executable and run it. | Test Linux builds have an Ubuntu 24.04 runtime baseline, not the project's eventual glibc 2.31 distribution target. PipeWire, ALSA, WebKitGTK 4.1 and tray libraries must be available. AppImage portability and FUSE/runtime requirements still need laptop validation. |

The application identifier remains com.socialspeak.desktop across builds.
Install only one version at a time. Test and future production builds share
application data; uninstalling the binary does not erase saved server/session
data. The window close button can hide to tray; choose Quit to stop the app.
Uninstall using Windows Settings → Apps, moving the macOS app to Trash, or
your Linux package manager (delete the AppImage for that format).

Open **Build → About this test build**, or the tray's **About this test build**,
to see the version/channel, full commit, target, profile and source state.
Include these details in every report. Ordinary developer builds that bypass
the packaging script explicitly report development/unknown provenance.

The macOS bundle declares microphone use and the audio-input entitlement.
Approve microphone or Input Monitoring only when testing the corresponding
feature. Ad-hoc signatures may need permissions granted again after a rebuild;
stable Developer ID signing and upgrade persistence remain T-057.

## Connecting and reporting

Real voice testing will need a reachable test server, its host/ports,
independently confirmed certificate fingerprint, and account/invite details.
Keep credentials out of screenshots and reports. A second installed client
and an audible participant are necessary for actual call acceptance.

Until T-076 lands, report the build identity, OS, devices, reproduction steps,
expected/actual behavior and whether the result was observed or measured.
Do not label network RTT or device estimates as measured mouth-to-ear latency.
Guided test runs and sanitized evidence ZIP exports are planned, not yet in
this package. Recording remains off by default and requires explicit consent.

## Acceptance record

T-075 notes in docs/plan/tasks/T-075-installable-test-builds.md track local
package verification separately from manual install/launch acceptance and
hosted build results. Production signing, automatic updates and stable release
approval remain T-057 and T-018.
