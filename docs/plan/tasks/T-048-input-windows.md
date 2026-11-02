# T-048 — `ss-input` Windows backend (low-level hooks / Raw Input)

Status: in-progress
Phase: 3
Size: S
Depends on: T-047
Paths: `crates/ss-input/src/windows/`

## Goal
Push-to-talk on any key or mouse button that still reaches the game.

## Scope
- Primary: Raw Input (`RegisterRawInputDevices` with `RIDEV_INPUTSINK` for
  keyboard + mouse on a hidden message-only window on a dedicated thread). No
  hook timeouts, no interference with other apps, passes everything through.
- Fallback (feature or runtime switch): `WH_KEYBOARD_LL` + `WH_MOUSE_LL`
  always calling `CallNextHookEx`; handle `WM_XBUTTONDOWN/UP` for Mouse4/5.
- Map scan codes → `Code`; distinguish left/right modifiers; handle
  `WM_KEYUP` loss on focus change by polling `GetAsyncKeyState` for the bound
  key every 250 ms while pressed (stuck-key guard).
- UIPI note: input to elevated windows isn't visible; detect
  (`IsUserAnAdmin` false + foreground window elevated) and surface
  `Caps::limitations` so the UI can show "PTT won't work while <game> runs as
  administrator".
- Unit test with synthetic `SendInput` on the CI Windows runner (headless
  session caveat: skip if no interactive window station).

## Out of scope
`uiAccess` manifest signing (backlog).

## Acceptance criteria
- Manual: bind Mouse5, play a game in fullscreen (e.g. a free title), PTT
  works and the game also receives the button.
- Pressing the key while the app is unfocused and releasing after a focus
  change never leaves PTT stuck.

## Notes from implementation
- Added `WinListener` and selected it from `Backend::detect` on Windows. Raw
  Input registers keyboard and mouse with `RIDEV_INPUTSINK` against a hidden
  message-only window owned by a dedicated thread. It never registers or
  consumes a hotkey; the explicit mock remains available for tests.
- Set 1 scan codes map to physical USB-HID `Code` values independently of the
  active keyboard layout. Extended keys, left/right modifiers, F13–F24, and
  Mouse4/5 are covered. The shared `PttStateMachine` retains the common 30 ms
  debounce and configurable release delay.
- Held inputs retain their side-specific virtual key and are checked with
  `GetAsyncKeyState` every 250 ms. A missing physical down state synthesizes
  the lost up edge, preventing a focus transition from leaving transmit open.
- Capabilities detect a non-elevated SocialSpeak process facing an elevated
  foreground application and name that window in the UIPI limitation shown to
  the settings UI.
- Windows-target compilation and strict clippy run on every existing Windows
  CI job. Unit tests cover scan/button mapping, reservations, and state-machine
  edges. A CI-only `SendInput` test exercises the native window when the runner
  exposes an interactive window station and otherwise records a permitted skip.
- Remaining before `done`: implement or explicitly waive the low-level-hook
  fallback, then run the fullscreen Mouse5/pass-through and focus-change manual
  acceptance checks on Windows. Those interactive checks have not been run on
  this macOS development host.
