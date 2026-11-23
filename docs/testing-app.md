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

How to add a server, talk, set up audio and push-to-talk, and use the
keyboard shortcuts is in the [user guide](user-guide.md). Sessions and server
order survive restarting the app. The native app-data directory contains
private JSON session and pin files, not an encrypted keychain. Passwords are
never saved. Do not include `sessions.json` in test reports. Actual gaming,
permission/revocation and compositor behavior still need the T-058 manual
matrix; device switching speed, permission flows and audible quality still
need real-machine validation.

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

Install the Rust toolchain pinned by rust-toolchain.toml, Node 26.13.0, pnpm 12.18.0,
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

Defaults: native macOS app ZIP + DMG, Windows x64 NSIS/MSI installers, Linux x64
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
publishes a release itself. Direct manual test runs do not use production signing
credentials; the release workflow can call it with signed updates enabled.

A successful build does not establish installed acceptance. Use the run records
and platform build identity when reporting Windows/Linux installation results.

## Install and identify

Extract the downloaded workflow archive first. Check SHA256SUMS before
installation: `shasum -a 256 <file>` on macOS, `sha256sum <file>` on Linux,
or `Get-FileHash <file> -Algorithm SHA256` in PowerShell; compare the digest.
Only install artifacts from a build/source you trust.

| OS | Installation | Permissions and caveats |
|---|---|---|
| Windows x64 | Run the NSIS setup .exe or MSI; launch Sideband from Start. | Unsigned test installers may trigger SmartScreen. Follow your machine's approved installation policy. WebView2 is required; the installer may need network access to install it. |
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

Open **Help** in the server rail and choose **Guided tests**. Select a checklist and **Start run**. The
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

## Adaptive screen-share quality (T-074)

A sharer now sends two versions of the stream: the full one (the quality
preset, for example 1080p30) and a low one (640×360, 15 fps, about
0.3 Mb/s). The server gives each viewer one of them. A viewer whose own
network falls behind moves to the low one, the other viewers keep full
quality, and the viewer moves back once its network recovers.

### What was measured

Measured 2026-11-16 on an M4 Pro MacBook Pro running macOS 27.0.1. The
server ran locally, and one of two viewers went through a network shaper:
30 s at 600 kb/s with 2 % loss and 40 ms extra delay. The full results,
with each run's figures, are in
[T-074's notes](plan/tasks/T-074-adaptive-bitrate.md#measurements).

| What | Target | Measured |
|---|---|---|
| Slow viewer shows the low stream | within 3 s | 1.0–1.2 s |
| Longest freeze after that | under 1 s | 0.3–0.4 s |
| Slow viewer back on full quality after the network recovers | within 30 s | 0.2–14 s |
| Healthy viewer during all of it | 30 fps, no skipped group | 29.4 fps or more, nothing skipped |
| Full-motion 1080p30 with both versions encoding | 30 fps | 29.1 fps or more |
| Sharer CPU with both versions | under 25 % of a core | about 4 % |
| Encode time per frame, both versions together | 15 ms (T-071) | 16–17.5 ms: **over target**, frame rate still held |
| Only one hardware encoder available | call goes on | passes; the slow viewer still had 9.4 s of gaps over 1 s in 30 s |
| Server upload per viewer | – | see [self-hosting](self-hosting.md#1-what-you-need) |

Not yet tested: the desktop viewer window on a really slow network,
Windows and Linux, and two machines through a VPS or over Wi-Fi.

### Test it in the app

You need three clients: one sharer and two viewers, one of them on a slow
network. macOS's Network Link Conditioner slows the whole Mac down, so the
slow viewer must be alone on a second Mac. The sharer and the healthy
viewer can share the first Mac as two installed clients.

1. **Server.** Use the test server, or run one from a checkout as in the
   [README](../README.md#quick-start). For a local server that other Macs
   reach, set `bind_quic` and `bind_http` to the first Mac's LAN address
   rather than `127.0.0.1`.
2. **Network shaping on the second Mac.** Install Network Link Conditioner
   (Apple's *Additional Tools for Xcode*) and add a custom profile:
   downlink 600 kb/s, 2 % downlink packet loss, 40 ms downlink delay.
   Leave it off for now.
3. **Share.** On the first Mac, share a screen at 1080p30. The share
   controls show a status line, for example `low rendition ready · 4.0 Mb/s`.
   If it says `one rendition (no second hardware encoder)`, note it: that
   machine only has the single-stream fallback.
4. **Watch.** Open the stream on both viewers. The statistics line in the
   viewer window (**View → Show stream statistics**, Cmd+I) shows which
   version is on screen (`full` or `low`), the frame rate and `dropped`.
5. **Slow one viewer down.** Turn the profile on on the second Mac. Check:
   - the slow viewer's statistics line says `low` within about 3 s, with no
     blank or stuck picture at the switch;
   - the sharer's status line shows `1 on low rendition`;
   - the healthy viewer stays on `full` at about 30 FPS, and its `dropped`
     count does not climb.
6. **Recover.** After about 30 s, turn the profile off. The slow viewer
   should be back on `full` within 30 s, again without a visible gap.
7. **Full motion.** Share a game or a full-screen video and check the
   healthy viewer still shows about 30 FPS.

Write down for each step what you saw: times, frame rates, any gap or
frozen picture, and the sharer's status line. Results go in the T-074 rows
of the [acceptance matrix](plan/tasks/ACCEPTANCE.md).

### Rerun the measurements

The scripted runs live in [`experiments/livekit-spike`](../experiments/livekit-spike/README.md#t-074-scenarios)
and use the real server and T-074's code. From that folder:

```sh
CARGO_TARGET_DIR=../../target cargo build --release --no-default-features
BIN=../../target/release/livekit-spike ./run.sh /tmp/t074 t074-impair
python3 -I analyze_t074.py /tmp/t074/t074-impair
```

The other scenarios are `t074-impair-gop2`, `t074-fallback`, `t074-hard`,
`t074-pattern`, `t074-screen` and `t074-screen-single`; the harness README
says what each one does.

## Acceptance record

T-075 notes in docs/plan/tasks/T-075-installable-test-builds.md track local
package verification separately from manual install/launch acceptance and
hosted build results. Signed updater packaging is implemented in T-057;
[release setup](release.md), installed upgrades and OS-signing acceptance remain.
Stable voice-quality approval remains T-018.
