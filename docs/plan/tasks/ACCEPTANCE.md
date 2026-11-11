# Acceptance matrix

Every hands-on check the plan still owes, on one page: what to do, which task
owns it, and on which platform. A task whose code is merged and whose only
open items are rows here has status `awaiting-acceptance` in
[INDEX.md](INDEX.md). Collected by T-099 on 2026-11-11 from every task file's
acceptance criteria and "Notes from implementation" at `0d0cb16`.

How to use it:

- The owning task file has the full steps; this page names them. Record
  results (build, OS version, hardware, outcome) in that task's notes, not
  here.
- When a cell passes, change it to `pass` here in the same commit. When every
  row of a task passes, set the task `done`.
- A failed check reopens the task (`in-progress`) or opens a new one.
- Order: T-081 to T-085 fix defects that would be scored as voice quality, so
  their rows come before listening tests and T-018 (see
  [05-testing-app.md](../05-testing-app.md)).

Cells: `open` not run; `part` some evidence recorded in the task, not all;
`pass` done; `–` does not apply. "Two clients" means two machines, ideally
with the server on the VPS.

## Voice path and audio devices

| Check | Task | macOS | Windows | Linux | Server / other |
|---|---|---|---|---|---|
| Typing, sending, mark-read and mute during a call cause no dropout or jitter growth (two clients, remote server) | [T-081](T-081-media-pump-independence.md) | open | open | open | VPS |
| Lid closed ≥ 10 s in a call: reconnects within about 1 s | [T-081](T-081-media-pump-independence.md) | open | – | – | – |
| Server restart mid-call: voice back at `Connected`, missed messages arrive after | [T-081](T-081-media-pump-independence.md) | open | open | open | – |
| Congested uplink (100 kb/s link conditioner): latency stays bounded | [T-081](T-081-media-pump-independence.md) | open | open | open | – |
| VAD word onsets heard; speaking ring and attenuation clear between words | [T-082](T-082-talk-burst-boundaries.md) | open | open | open | – |
| PTT release and mute mid-word: the tail stops cleanly | [T-082](T-082-talk-burst-boundaries.md) | open | open | open | – |
| Screen share with VAD: lip sync does not drift over minutes | [T-082](T-082-talk-burst-boundaries.md) | open | open | open | – |
| Hour-long call: `playback.queueMs` about 25 ms or less, `overflow` stays 0 | [T-083](T-083-playback-pacing-lag-recovery.md) | open | open | open | – |
| 44.1 kHz USB output: queue in band, left and right correct | [T-083](T-083-playback-pacing-lag-recovery.md) | open | open | open | – |
| Apply, suppressor switch and headset replug during music: playback catches up; forced cuts are short dips, no click | [T-083](T-083-playback-pacing-lag-recovery.md) | open | open | open | – |
| AEC on speakers converges at least as well as before | [T-083](T-083-playback-pacing-lag-recovery.md) | open | open | open | – |
| Voice worker log shows MMCSS promotion | [T-083](T-083-playback-pacing-lag-recovery.md) | – | open | – | – |
| USB headset unplug/replug script (8 steps), portable backend and Apple voice processing | [T-084](T-084-device-reopen-retry.md) | open | [T-011](T-011-audio-windows-verification.md) | [T-012](T-012-audio-linux-verification.md) | – |
| Mute while connecting to an unreachable second server; mute with the network off: dock, tray and peers agree | [T-085](T-085-mute-fail-closed-shortcuts.md) | open | open | open | – |
| Global PTT while history or a send is slow; rapid in-app PTT taps never stick; disconnect during a queued send does not rejoin voice | [T-085](T-085-mute-fail-closed-shortcuts.md) | open | open | open | – |
| One account on two devices: seat takeover survives a > 1 min outage; a single device rejoins after > 1 min offline | [T-086](T-086-connection-identity-track-reuse.md) | open | open | open | – |
| Server restart, ≥ 3 people rejoin in another order: rings and volume follow the right person; the desktop call stays up | [T-086](T-086-connection-identity-track-reuse.md) | open | open | open | – |
| Channel switched between `voice` and `low_latency` mid-call: old and new members hear each other | [T-086](T-086-connection-identity-track-reuse.md) | open | open | open | – |
| Two- or eight-input USB interface: Input 1, Average, Input 2, switching mid-call, mono fallback logs `input=1` | [T-088](T-088-capture-channel-voiceio-frames.md) | open | – | – | – |
| VoiceIO: left-only tone 10 min never swaps; overruns 0; 10 toggles keep memory flat; replug reopens on the new default | [T-088](T-088-capture-channel-voiceio-frames.md) | open | – | – | – |
| Broken IPv6 on a dual-stack host: connects in about 3 s | [T-089](T-089-reconnect-policy.md) | open | open | open | – |
| Server restart with > 30 clients behind one address: all reconnect | [T-089](T-089-reconnect-policy.md) | – | – | – | VPS |
| Removed server's token is refused; `ss-server user sessions revoke` drops a live client within 30 s, no reconnect loop | [T-093](T-093-session-revocation.md) | open | open | open | server |
| Audio start to speaking ring under 100 ms (two clients) | [T-044](T-044-channel-voice-ui.md) | open | open | open | – |
| Device switch mid-call under 1 s; loopback and cues listened to; permission denial; unplug/replug | [T-051](T-051-audio-settings-ui.md) | open | open | open | – |
| Apple voice processing mode from settings | [T-051](T-051-audio-settings-ui.md) | open | – | – | – |
| Diagnostics panel matches `ss-cli` stats on the same real call; hardware routes | [T-063](T-063-diagnostics.md) | open | open | open | – |
| Platform audio verification (latency, IAudioClient3, ducking, MMCSS; PipeWire properties, fallback) | [T-011](T-011-audio-windows-verification.md), [T-012](T-012-audio-linux-verification.md) | – | open | open | – |
| macOS acoustic round trip, AirPods, USB interface, stored "Don't Allow", VoiceIO A/B, timestamp decision | [T-019](T-019-audio-macos-acceptance.md) | open | – | – | – |
| Voice-quality release gate: blind A/B against TS3, latency, impairment, lock defaults (≥ 3 people) | [T-018](T-018-spike-evaluation.md) | open | open | open | VPS |

## Push-to-talk and shortcuts

| Check | Task | macOS | Windows | Linux | Server / other |
|---|---|---|---|---|---|
| Mouse5 bound in a fullscreen game: PTT works and the game still gets it; release after a focus change never sticks | [T-048](T-048-input-windows.md) | – | open | – | – |
| `macos_ptt` denied-permission path, then grant; F13 and Mouse4 pass through in a fullscreen game; left/right modifiers | [T-049](T-049-input-macos.md) | open | – | – | – |
| Revoke Input Monitoring or enter a secure field while held: `Released`, clear state, no silent reopen | [T-049](T-049-input-macos.md) | open | – | – | – |
| X11 XI2, KDE Wayland, GNOME 48+, Hyprland `bindn`, Sway "unavailable", opt-in evdev (`crates/ss-input/linux.md`) | [T-050](T-050-input-linux.md) | – | – | open | – |
| Global-shortcut matrix per platform and desktop, then the common list (no bindings no listener, three actions, gaming, tray, password field, capture, revoke, reconnect) | [T-058](T-058-global-app-shortcuts.md) | open | open | open | – |
| Stuck-key reconcile: swallowed keys (AutoHotkey, PowerToys), XI 2.1, sleep (code first) | [T-087](T-087-input-stuck-key-reconcile.md) | open | open | open | – |

## Desktop app

| Check | Task | macOS | Windows | Linux | Server / other |
|---|---|---|---|---|---|
| Onboarding script (6 steps: trust, invite, saved session, second server, restart and replaced certificate, keyboard-only) | [T-043](T-043-onboarding-ui.md) | part | open | open | – |
| 50×30 fixture scrolls smoothly; moderator moves and reconnect in installed builds; restricted account's disabled actions | [T-044](T-044-channel-voice-ui.md) | open | open | open | – |
| PTT release, focus loss, mute and deafen silence transmission | [T-044](T-044-channel-voice-ui.md) | open | open | open | – |
| 10k cached messages scroll at 60 fps on a mid laptop | [T-045](T-045-text-chat-ui.md), [T-045B](T-045B-rich-chat-composer.md) | open | open | open | – |
| Installed-app keyboard navigation and native link opening | [T-045B](T-045B-rich-chat-composer.md) | open | open | open | – |
| File picker and drag/drop uploads, clipboard files, `Downloads/Sideband downloads`, audio/video playback | [T-045C](T-045C-chat-attachments.md) | open | open | open | – |
| Notifications from the background (mention, DM, click-through, badges), then DND, server mute, channel levels, reconnect, revoked access, multiple servers | [T-055](T-055-notifications-sounds.md) | open | open | open | – |
| Every cue heard (DM outside voice, output device, zero volume, deafen, lost/restored); trigger to onset under 50 ms | [T-055](T-055-notifications-sounds.md) | open | open | open | – |
| Chat, rings, level meter and notifications still update with no ACL denial; window size and position survive restart; packaged build has no event inspector | [T-095](T-095-desktop-shell-hardening.md) | open | open | open | – |
| Dropping a file on the composer attaches it; middle-clicking a link opens no window | [T-095](T-095-desktop-shell-hardening.md) | – | open | – | – |
| Fresh install of a labelled test build; downloaded-artifact Gatekeeper result; AppImage on older distributions | [T-075](T-075-installable-test-builds.md) | open | open | open | – |
| Guided test run and evidence export without a terminal, with a second participant (focus, device, sleep, reconnect) | [T-076](T-076-guided-voice-testing.md) | part | open | open | – |

## Screen share

| Check | Task | macOS | Windows | Linux | Server / other |
|---|---|---|---|---|---|
| Interactive source picker; 1080p30 with encode under 15 ms and under 25 % of a core (macOS needs a Mac that accepts 1080p sessions) | [T-071](T-071-screen-capture-encode.md) | open | open | X11, Wayland open | – |
| Keyframe request yields an IDR within 100 ms on hardware | [T-071](T-071-screen-capture-encode.md) | open | open | open | – |
| Installed decoder backend and window controls; two viewer windows; recovery from decoder or network loss | [T-072](T-072-stream-viewer.md) | open | open | open | – |
| A/V skew within ±60 ms; latency under 150 ms on a LAN, under 300 ms through the VPS | [T-072](T-072-stream-viewer.md) | open | open | open | VPS |
| Grant and deny capture permission; share a display and a window; watch from another computer; reopen, cancel, stop, leave or switch voice | [T-073](T-073-screen-share-ui.md) | open | open | open | – |

## Server, VPS and load

| Check | Task | macOS | Windows | Linux | Server / other |
|---|---|---|---|---|---|
| 200 sessions: DM rename, then 50 role moves (read-pool wait, CPU); 4,000 group DMs Hello time; slow write plus chat burst, `socialspeak_slow_disconnects` 0 | [T-091](T-091-dm-welcome-fanout-limits.md) | – | – | – | VPS open |
| Load validation (2 h soak, 100 users with `perf`, worker threads, storms, UDP buffer errors, RSS); includes T-094's datagram latency rerun | [T-069](T-069-vps-load-validation.md) | – | – | – | VPS open |
| Backup and restore with ownership recorded; renewal hook with a real certificate; `nginx -t` with `limit_req_zone` and `upstream` | [T-098](T-098-deploy-ci-hygiene.md) | – | – | – | VPS open |
| Grafana/Prometheus dashboard imported into a running deployment | [T-063](T-063-diagnostics.md) | – | – | – | VPS open |
| One hour per fuzz target; second-person review of `docs/security.md`; MPL-2.0 exception decision | [T-067](T-067-security-acceptance.md) | – | – | – | open |

## Release and GitHub (maintainer)

| Check | Task | macOS | Windows | Linux | Server / other |
|---|---|---|---|---|---|
| Regenerate `pnpm-lock.yaml` with the pinned Node and pnpm; `pnpm install --frozen-lockfile` and `pnpm check` leave git clean | [T-098](T-098-deploy-ci-hygiene.md), [T-095](T-095-desktop-shell-hardening.md) | open | – | – | – |
| Dispatch `ci.yml` (first Linux run of the feature steps and Compose smoke, first `windows-2025`) and `public-repo.yml` with Gitleaks | [T-098](T-098-deploy-ci-hygiene.md) | – | – | – | GitHub open |
| Dispatch `fuzz.yml` with every target | [T-101](T-101-router-tolerance-parser-fuzzing.md), [T-067](T-067-security-acceptance.md) | – | – | – | GitHub open |
| Dispatch `ci.yml`: `cargo deny` passes with the dated RUSTSEC-2026-0459 ignore | [T-110](T-110-ci-release-followups.md) | – | – | – | GitHub open |
| Dispatch `fuzz.yml`: all seven targets on `ubuntu-24.04`, locked fetch passes, `fuzz/Cargo.lock` unchanged; Dependabot accepts `/.github/buildkit` | [T-110](T-110-ci-release-followups.md) | – | – | – | GitHub open |
| Environments `desktop-release`, `desktop-signing`, `server-release` with secrets moved in; rulesets for `main` and `server-v*` | [T-097](T-097-release-pipeline-gating.md) | – | – | – | GitHub open |
| Desktop release from a non-main branch fails; `server-v0.1.0` dry run, re-dispatch without `force` fails; first update-enabled release with no signing variables in the log and `gh attestation verify` | [T-097](T-097-release-pipeline-gating.md) | – | – | – | GitHub open |
| Signing credentials provisioned; fresh install and N→N+1 updater upgrade of every package | [T-057](T-057-packaging-updater.md) | open | open | open | GitHub |
| Developer ID notarization `Accepted`; `spctl` and `stapler validate` pass on the DMG; Input Monitoring survives an update | [T-057](T-057-packaging-updater.md), [T-097](T-097-release-pipeline-gating.md) | open | – | – | – |

## Maintainer decisions and reviews

Not hands-on tests, but they keep tasks open:

| Item | Task |
|---|---|
| Ban and `MemberLeft`; owner above administrators; Linux rtkit priority; capture channel count; PTT arming rule; Windows hook fallback | [T-106](T-106-review-decisions.md) |
| Review of this reconciliation (plan documents, statuses, this page) | [T-099](T-099-plan-doc-reconciliation.md) |
| Second-person review of `docs/security.md` | [T-067](T-067-security-acceptance.md) |
