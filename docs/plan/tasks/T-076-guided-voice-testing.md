# T-076 — Guided voice tests and local evidence bundles

Status: todo
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
