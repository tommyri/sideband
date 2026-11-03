# T-076 — Guided voice tests and local evidence bundles

Status: in-progress
Phase: 3
Size: M
Depends on: T-075, T-043, T-044, T-051, T-058
Paths: `apps/desktop/src/features/testing/`, `apps/desktop/src-tauri/src/`, `crates/ss-client-core/src/`, `docs/testing-app.md`

## Goal
A tester follows an in-app checklist and exports evidence that lets another
person reproduce or diagnose a result without guessing the build or settings.

## Scope
- Follow the report fields, privacy rules and test workflow in
  `../05-testing-app.md`; create a versioned JSON schema and bounded local
  recorder. The schema/test definitions can start before UI dependencies land.
- Show device/backend/format/permission information and voice statistics,
  with explicit units, sampling windows, availability and measurement origin.
  RTT and device estimates are never presented as measured acoustic latency.
- Guided PTT/fullscreen/focus, capture/permissions, hotplug/sleep/reconnect
  checks. Record configured-shortcut and transmit edges only, not arbitrary
  global input. Capture user observations and pass/fail/not-run outcomes.
- Controlled voice trials with preset/condition IDs and subjective scores;
  optional recordings require explicit participant consent and a separate
  export choice. No recording by default or automatic upload.
- Provide the acoustic measurement procedure and record calibration and
  evidence when performed; otherwise leave mouth-to-ear latency unmeasured.
- Preview and export a ZIP containing JSON results, sanitized logs and user
  notes. Redact credentials/tokens/invites, omit message contents, and allow
  omission of identifying device/network details.
- Link reports to the relevant acceptance task and full T-075 build identity.
  T-063 reuses this collector/export format for its broader diagnostics UI.

## Acceptance criteria
- On an installed build, complete a guided PTT/focus or device-recovery run
  and export a self-contained report without a terminal.
- Tests cover schema compatibility, ordering, bounded retention, missing
  values, outcome labels and redaction using seeded sensitive values.
- Verify no recordings are produced or exported without the respective
  explicit choices. Manual hearing/game behavior is never auto-marked passed.
- Collect real cross-OS reports for the platform acceptance tasks; preserve
  unresolved failures. T-018 retains its independent group go/no-go.

## Notes from implementation


Implemented (2026-11-03): guided PTT/focus, device recovery, controlled listening
and acoustic-measurement checklists; version-1 JSON schema; bounded local semantic
recorder; human outcomes/notes/scores; full build identity; start/end settings,
permissions and unavailable-field reasons; preview with default device omission
and redaction; local ZIP export. The installed app needs no terminal to make a
report. T-063 can reuse `features/testing/report.ts`, the schema and native ZIP
export command.

Recording is an explicitly consented external WAV attachment, not an in-app
microphone recorder. Attachment and export are separate choices; native validation
enforces both. No recordings or reports are uploaded automatically. Actual device
format/buffer sizes are not exposed by the current audio API and are reported as
unavailable; backend selection is labelled requested. Webview receipt timestamps
and 40 ms transmit indicators are not acoustic or hardware-precision measurements.

Validation: frontend tests cover schema version/outcomes, ordering/retention,
missing measurements, seeded secret redaction, event allowlisting, acoustic
calibration and the finish/preview/export UI. Native tests check ZIP CRC/layout,
path restrictions, future schema rejection and recording consent/export gates.
Packaged macOS UI smoke created a ZIP in Downloads; Python independently verified
its CRCs, expected files, full build identity, omitted device details, unmeasured
acoustic latency and not-run outcomes. No microphone/Input Monitoring permission
was granted and no hearing/game acceptance was inferred from this smoke test.

Remaining acceptance: collect real Windows, macOS and Linux reports with a second
participant, including fullscreen/focus and device/sleep/reconnect failures. Keep
this task in-progress until those reports are collected; T-011/T-012/T-019 and
T-048/T-049/T-050/T-058 retain their own acceptance, and T-018 its group release gate.
