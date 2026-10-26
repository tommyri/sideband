# T-048 — `ss-input` Windows backend (low-level hooks / Raw Input)

Status: todo
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
