# Sideband testing builds

For downloadable versioned installers, use the manual
[desktop release workflow](desktop-releases.md). It supports OS selection and
replacement of an existing development version.

These are internal test builds, not stable releases. T-018's voice comparison
is still a release gate. Packages now include server onboarding (T-043): add
an address, compare and trust its certificate fingerprint, register with an
invite or log in, and reconnect with a saved session. Connected servers show live channels, members and voice controls (T-044).
Audio settings (T-051), global shortcuts (T-058), and guided reports (T-076)
are available.
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
voice or **Disconnect** to close the server connection. Ctrl/Cmd+Shift+M toggles mute; Ctrl/Cmd+Shift+Alt+D toggles deafen; Ctrl/Cmd+Shift+Up/Down select channels. Interaction settings offer
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

## Live diagnostics

Open **Audio settings → Diagnostics**, or press **Ctrl+Shift+D**. The built-in
deafen shortcut is now **Ctrl/Cmd+Shift+Alt+D** to avoid that conflict. Configured
global bindings are unchanged. The panel inspects the active call, or the selected
server when no call is active. Changing server resets history; closing stops
collection. It does not request microphone or global-input permission.

The panel keeps up to 120 samples over the last 60 seconds, polling about every
500 ms. Graph gaps denote unavailable or delayed samples. QUIC RTT, cumulative
path packet loss/sent counts, congestion window, UDP-payload path MTU and application
datagram ceiling come from the live transport. They are different quantities from
audio frame loss/PLC/late counts and per-speaker applied jitter delay. Counters reset
on reconnect or stream reopen. Encoder bitrate is the configured/adapted encoder
rate, not measured wire throughput. FEC shows the active channel profile's setting.

Actual CPAL device sample rates and backend, software stage timing, ring overruns
and underruns, selected devices/settings, and the input backend's last ten semantic
events are shown. CPU figures are last/mean/max processing time per 10 ms frame,
not total process CPU. Apple system-stage CPU and device rates are unavailable
when the native API does not expose them. Hardware buffer sizes and migration count
are explicitly unavailable. Mouth-to-ear latency is unmeasured unless you perform
the separate calibrated guided test; no transport estimate substitutes for it.

**Preview redacted diagnostics** freezes a report for **Copy diagnostics** or
**Save log bundle**. It reuses the T-076 versioned schema, bounded recorder,
redaction and local ZIP export; raw logs, account identities, addresses, arbitrary
keys and recordings are excluded. Review free text before sharing. Notes and
measurements never auto-mark an audible test passed. Server operators can use
`ss-server conn list --data-dir ./data` and the dashboard in `deploy/grafana/`.

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
| Windows x64 | Run the NSIS setup .exe; launch Sideband from Start. | Unsigned test installers may trigger SmartScreen. Follow your machine's approved installation policy. WebView2 is required; the installer may need network access to install it. |
| macOS 13+, Apple Silicon | Open the DMG and copy Sideband to Applications, or expand the app ZIP and move the app there. Quit an older copy first. | Test builds are ad-hoc signed, not notarized. Gatekeeper may require an explicit approval through System Settings → Privacy & Security. Do not disable Gatekeeper or reset TCC. |
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

Open **Guided tests** from the app, choose a checklist and **Start run**. The
available checks cover PTT/fullscreen/focus, devices/permission/recovery,
controlled listening trials and calibrated acoustic measurements. Connect and
configure audio first. Close the panel to exercise voice controls, system settings
and other apps; collection continues. Reopen it to enter outcomes and notes,
then **Finish run → Preview sanitized bundle → Export ZIP to Downloads**.
The path is shown in the app (`Downloads/Sideband reports`). Share the ZIP
only when you choose. No network upload is performed.

Each step defaults to **not-run**. Only the tester can mark pass or fail; automatic
statistics never establish hearing quality, game pass-through or recovery success.
Record failures and reproduction conditions, including approximate recovery time.
The initial server is retained as the event source across reconnects. Start a new
run when testing another server, preset or condition. New runs replace the in-memory
run; export first. Closing the app loses unexported evidence.

Runs stop after 30 minutes and retain the last 500 events, with dropped-event
counts, monotonic receipt offsets and UTC start/end timestamps. Event payloads
are capped at 4096 characters and statistics at 32 speakers with omission counts.
Webview suspension can delay delivery; timing is diagnostic, not real-time proof.
Statistics originate in the voice core at 500 ms cadence. RTT is smoothed QUIC
network round-trip, jitter delay is applied receiver buffering, processing latency
is a chain estimate, and ring/loss/PLC/over-budget counters are cumulative for the
current worker/track (they reset on reopen). RMS is linear mixed-output amplitude.
Local transmit indicators use the native 40 ms speaking observation; they do not
prove remote audibility. Negotiated device format/buffer sizes are currently
unavailable and explicitly labelled. Device backend selection is a request, not
confirmation of the actual opened route. Missing statistics carry a reason.

The ZIP contains `report.json`, the versioned `schema.json`, `diagnostics.log`
(allowlisted semantic events only), and `notes.txt`. Full T-075 build identity,
settings at start/end, settings changes, permission/device/connection events,
configured shortcut actions and human observations are included. Server addresses,
user IDs, credentials, chat contents and arbitrary keys are never collected.
Identifying device details are omitted by default. Common credential patterns,
URLs, email and IPv4 addresses are redacted from text; inspect the preview because
unlabelled secrets or identifying prose cannot be reliably recognized. Do not
paste messages, passwords, tokens or invites into notes.

Controlled trials record preset/condition IDs and per-listener quality, effort
and delay scores (1 worst, 5 best). IDs do not apply settings automatically: apply
the desired settings before each run. Use one report per listener/condition with
consistent passage, room/noise, devices and volume. T-018 still requires at least
three people and its independent group go/no-go.

The app does not record audio. Optional externally recorded WAV evidence (up to
10 MiB) requires an explicit participant-consent checkbox before attachment and
a separate **Include the consented WAV in this export** choice, off by default.
Revoking consent clears the attachment. Audio and WAV metadata can identify people
and are not redacted; inspect the original before including it. The native exporter
also enforces both choices. Export size is limited to 16 MiB.

For acoustic latency, use a two-channel recorder sharing one clock: channel A at
the sending microphone, channel B at the receiving output. First capture the same
impulse on both channels to calibrate channel skew. Record routing, distances,
sample rate and calibration. Send several impulses through the call, preserve
failures, and locate departure/arrival sample indices in the original waveform.
Enter rate, indices, skew and an evidence reference per trial; the report calculates
`(arrival - departure - skew) / sampleRate × 1000` ms and labels it human supplied,
not independently verified. Without complete calibrated evidence, mouth-to-ear
latency stays unmeasured. Do not substitute network RTT or device estimates.

## Acceptance record

T-075 notes in docs/plan/tasks/T-075-installable-test-builds.md track local
package verification separately from manual install/launch acceptance and
hosted build results. Production signing, automatic updates and stable release
approval remain T-057 and T-018.
