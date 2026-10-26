# T-049 — `ss-input` macOS backend (listen-only CGEventTap, Input Monitoring)

Status: todo
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
