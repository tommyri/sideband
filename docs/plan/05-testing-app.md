# Installable testing-app milestone

Decision: 2026-11-02. Build the app needed to evaluate the voice foundation.
T-018 now gates voice-quality release approval, not UI work or test builds.
Existing quality targets, opt-in processing defaults and manual acceptance
requirements remain unchanged.

## Delivery order

| Priority | Task / increment | Usable outcome |
|---|---|---|
| 1 | T-075 — installable test builds | Repeatable Windows, macOS and Linux test packages with exact build identity and installation instructions. Start with the existing scaffold; this is not yet the completed voice-testing app. |
| 2 | T-043 — onboarding, first increment | Add server, explicit certificate trust, register/login and reconnect without a terminal. Server-rail polish may follow the essential flow. |
| 3 | T-044 — voice controls, first increment | Channel list, join/leave, members, speaking state, mute/deafen and connection state. Advanced moderation menus may follow. |
| 4 | T-051 + T-058 — essential voice setup | Input/output device selection, PTT vs VAD, bind capture or portal settings, honest permission/capability notices, release on disconnect. |
| 5 | T-076 — guided tests and evidence | Local diagnostics, reproducible test runs, observations and redacted export bundles. Its report schema and test scripts can be built alongside priorities 2–4. |
| 6 | Acceptance with the installed app | Run T-011/T-012/T-019, T-048/T-049/T-050 and T-018 on the user's Windows gaming PC, Mac and Linux laptop; fix failures with attached evidence. |

Review of 2026-11-09: before priority 6, complete T-081 to T-085 (audio
stalls during control commands, voice-activation burst boundaries, playback
delay growth, device reopen, mute and shortcut reliability). Evidence
collected before them would record those defects, not the voice design.

Packaging proceeds alongside UI increments after the first pipeline lands.
No dependency on text chat (T-045), DMs, admin UI, production signing,
automatic updates or screen sharing is introduced for this milestone.
T-057 owns production distribution, signing/notarization and updater
acceptance after reusing T-075's packaging work. The milestone does not
claim that an unsigned/ad-hoc build has passed production trust checks.

## Implementation prerequisites versus acceptance

Implemented backends T-048/T-049/T-050 may be integrated now. Their pending
interactive tests are outcomes of this milestone, not prerequisites to
building the app that runs those tests. T-058 tracks integration separately.
Similarly, the essential implemented UI increments can feed T-076 before
all UI polish or cross-OS manual acceptance is complete. Keep each original
task in progress until its full acceptance criteria are satisfied.

## Evidence workflow

Install a labelled build → connect to a test server → choose a guided test
→ run the steps → record pass/fail/not-run and observations → export a ZIP.
The user explicitly chooses whether to share that file; no automatic upload.

The report records a schema version, test/run IDs, timestamps and monotonic
event offsets, version/commit/platform/architecture, device/backend/format
settings, permissions, voice parameters, bounded statistics and event history.
Unavailable fields include a reason rather than a fabricated value.

Tests distinguish automatic observations from human confirmation:

- Device/backend checks: selected devices, rates, buffer sizes, permission
  failures and stream reopens.
- Voice health: RTT, loss, jitter-buffer behavior, underruns, processing and
  reconnects. These are not labelled mouth-to-ear latency.
- PTT/game checks: only configured shortcut edges and transmit state; the
  tester confirms pass-through to the game and release across focus changes.
- Hotplug/sleep checks: prompts, device/connection timeline, recovery time
  and the tester's audible result.
- Controlled listening trials: preset/condition IDs, reproducible settings,
  subjective scores, and explicitly consented optional recordings.
- Acoustic latency: a documented measurement procedure, calibration and raw
  measurement provenance; otherwise mark it not measured.

Exports contain structured results, sanitized diagnostics/logs and the
tester's observations. Exclude credentials, authentication/invite tokens,
message contents and raw keystrokes. Recordings are off by default, require
explicit participant consent, and require a separate export choice. Preview
the contents and let the tester omit identifying device/network information.

## Milestone acceptance

- Install and launch on Windows x64, macOS Apple Silicon (Intel build if
  needed), and Linux x64 using documented test-build installation steps.
- Two installed clients connect to the test server, join voice and use their
  selected devices and supported transmit mode.
- A tester can complete a PTT/focus or device-recovery checklist and export
  evidence tied to the exact build without developer tooling.
- Export redaction, event ordering, missing-field handling and report schema
  have automated tests. Installation/listening results remain manual evidence.
- T-018's comparison still requires at least three people and a recorded
  go/no-go before voice-quality release approval.
