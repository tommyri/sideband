# T-058 — Global shortcuts for mute/deafen and PTT wiring in the app

Status: todo
Phase: 3
Size: S
Depends on: T-047, T-048, T-049, T-050, T-044, T-051
Paths: `apps/desktop/src-tauri/src/input.rs`, `apps/desktop/src/features/settings/shortcuts/`

## Goal
Wire `ss-input` into the desktop app: PTT, toggle mute, toggle deafen work
while gaming, with per-OS behaviour surfaced honestly.

## Scope
- `src-tauri` starts the `ss-input` backend on app start when any global
  binding is configured; forwards `PttEvent` to `VoiceSession::set_ptt`,
  toggles to the client core.
- Shortcuts settings page: bindings list with capture buttons, conflict
  warning (same binding twice), platform notice panel from `Caps`.
- Suspend the listener when the app is in "binding capture" mode and when a
  password field is focused (macOS secure input).
- Tray menu mirrors mute/deafen state.
- Telemetry-free diagnostics: last 10 input events view for troubleshooting.

## Out of scope
Scripting/macros.

## Acceptance criteria
- Manual matrix recorded: Windows, macOS, X11, KDE Wayland, GNOME Wayland,
  Hyprland, Sway (unavailable path) — PTT and toggles behave per `Caps`.
- No input processing when no bindings are configured (listener not
  started; verify no permission prompt on macOS in that case).

## Notes from implementation
