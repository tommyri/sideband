# T-049 — `ss-input` macOS backend (listen-only CGEventTap, Input Monitoring)

Status: in-progress
Phase: 3
Size: S
Depends on: T-047
Paths: `crates/ss-input/src/macos/`

## Goal
Push-to-talk on keys, modifiers and mouse buttons with the lesser Input
Monitoring permission and full pass-through.

## Scope
- `CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap,
  kCGEventTapOptionListenOnly, mask: keyDown|keyUp|flagsChanged|
  otherMouseDown|otherMouseUp|leftMouseDown/Up|rightMouseDown/Up)` via
  `objc2-core-graphics`; run loop source on a dedicated thread's run loop
  (not the main thread, to keep Tauri's loop free). Callback does nothing but
  push to a channel.
- Permission flow: `CGPreflightListenEventAccess()`; if false,
  `CGRequestListenEventAccess()` and return `Err(PermissionRequired)` with a
  deep link to the Input Monitoring pane; recheck on app focus.
- Re-enable on `kCGEventTapDisabledByTimeout` /
  `kCGEventTapDisabledByUserInput` and call `force_release()`.
- Key code → `Code` mapping (Carbon virtual key codes); modifier edges from
  `flagsChanged`; mouse button number from `kCGMouseEventButtonNumber`.
- Document code-signing requirement: TCC grants are tied to the signing
  identity; dev builds must use a stable ad-hoc or developer identity
  (coordinate with T-057).

## Out of scope
Secure-input fields (when a password field is focused macOS suspends taps;
just surface it).

## Acceptance criteria
- Manual: bind Mouse4 and `F13`; works while a fullscreen game has focus;
  key still reaches the game.
- Revoking Input Monitoring at runtime produces a clear UI state, not a
  crash.

## Notes from implementation
- Implemented and integrated from `task/T-049-input-macos`; remains in-progress
  pending the manual acceptance checks below. No Accessibility permission or
  hotkey registration is used. `Backend::detect` now selects `MacListener` on macOS;
  Windows/Linux retain the mock and the explicit mock remains usable everywhere.
- The session/head-insert/listen-only event tap and its CF run loop live on a
  dedicated thread. The C callback copies event fields to a 256-entry ring and
  returns the original event pointer. State-machine work and the user's handler
  run after the callback returns. Repeated downs are ignored. Carbon virtual
  keys map to physical USB HID codes; side-specific modifier flags are tracked
  independently, including modifiers held before capture started. Mouse button
  numbers 0–4 map to Left/Right/Middle/Mouse4/Mouse5. Fn/media and unmapped
  JIS-only keys are ignored; Caps Lock follows its locked state.
- Start/capture preflight Input Monitoring; if missing, request it once for
  that attempt and return PermissionRequired with the Privacy_ListenEvent
  settings deep link. Capability checks never prompt. The app rechecks them
  on focus. T-058 will wire those focus events into the T-042 desktop shell.
- Disabled-by-timeout/user-input notifications and ring overflow force-release,
  discard stale queued input, repair missed ups from physical state, and
  re-enable the tap. Inputs still held stay suppressed until their matching up.
  Interrupted gesture capture is cancelled instead of saving a partial chord.
- Runtime permission and secure-input checks run every 250 ms. They release
  transmit, stop the native tap, and appear through the additive
  `Handle::status()` API. The app drops/restarts the handle after access returns.
  Run-loop polling caps command/release-timer wakeup delay at 10 ms when the
  handler is responsive. Keep handlers short; controls/capture block and belong
  off the UI/async-runtime thread. Reentrant synchronous controls return an
  error. Dropping from a handler requests shutdown without joining itself.
- Tap/context resources are torn down on their owning thread, including startup
  failures. A shared reservation covers both gesture capture and running mode,
  across listener clones, and lasts until the worker has released its source.
- Tests use synthetic event fields and an in-memory CGEvent, never posted input
  or a live tap. They cover pass-through identity, bounded overflow, F13/Mouse4,
  both modifier sides, capture, repeat suppression, missed-up recovery, timing,
  reservations, cancellation, reentrant controls, and runtime error reporting.

### Manual acceptance still required

Use a stable bundle identifier, installation path, and signing identity for TCC.
A stable Developer ID/development identity is preferable; ad-hoc rebuilds can
invalidate the grant and need to be approved again. T-057 must preserve this
identity. Do not reset TCC or silently substitute Accessibility permission.

1. Run `cargo run -p ss-input --example macos_ptt` deliberately. If denied,
   verify the Input Monitoring error/link, grant access in System Settings,
   then restart the example. Detection alone must not prompt.
2. Hold F13 and Mouse4 separately while a fullscreen game is focused. Check one
   Pressed/Released pair and verify the same input still reaches the game.
   Repeat with left/right modifiers; `... --example macos_ptt -- capture`
   records a chord for checking the capture display.
3. Revoke Input Monitoring during a held bind. Confirm Released and a clear
   permission state (macOS may also require/restart the process). Restore access
   and restart; the held key must not silently reopen transmit.
4. Enter a secure password field while held. Confirm release and SecureInput;
   leave the field and restart. Stop/drop while held must also release.

These interactive game/TCC checks have **not** been run by the agent. They are
not replaced by synthetic tests. The smoke example exists for this acceptance
pass and is not run by CI.

References: [Apple CGEvent](https://developer.apple.com/documentation/coregraphics/cgevent),
[listen-only taps](https://developer.apple.com/documentation/coregraphics/cgeventtapoptions/listenonly).
Key/side-mask mappings were checked against the installed macOS SDK's
`HIToolbox/Events.h` and `IOKit/hidsystem/IOLLEvent.h`; bindings use the objc2
framework family, with a narrow Carbon query for secure-input state.

Validation on this Apple Silicon macOS host: `just check` passed (261 tests,
1 manual load test ignored), `just check-deps` passed, and
`git diff --check` passed. The ss-input run includes 37 unit tests plus one
doctest. Windows/Linux execution is left to the existing CI matrix; their
backend remains the mock and framework dependencies are macOS-only.
