# T-058 — Global shortcuts for mute/deafen and PTT wiring in the app

Status: in-progress
Phase: 3
Size: S
Depends on: T-047, T-044, T-051
Paths: `apps/desktop/src-tauri/src/input.rs`, `apps/desktop/src/features/settings/shortcuts/`

## Goal
Wire `ss-input` into the desktop app: PTT, toggle mute, toggle deafen work
while gaming, with per-OS behaviour surfaced honestly.

Integrate the implemented T-048/T-049/T-050 interfaces now. Their manual
acceptance remains open and is exercised using this app and T-076; it is not
a prerequisite to building that test path. Missing capabilities stay explicit.

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

Implemented (2026-11-03):
- One native listener routes independent PTT/mute/deafen actions; legacy binding
  JSON remains PTT. Toggles fire on fresh presses, not key repeats. The existing
  Wayland portal callbacks route the same actions with compositor-owned bindings.
- Global preferences persist in `shortcuts.json`. Empty preferences never start
  listening. Explicit capture is temporary, cancellable and bounded to 15 seconds.
  Captured drafts do not change saved bindings until Save. Duplicate/overlapping
  trigger keys are rejected in UI and native validation.
- Settings expose runtime Caps, Input Monitoring settings, portal configuration,
  and explicit Linux evdev fallback opt-in. evdev never changes OS permissions.
  Unavailable backends disable capture. Listener errors remain visible and can
  be retried after fixing permissions. Raw listeners use native key/mouse capture.
- Capture and password/in-app-binding-capture focus suspend listening and close
  PTT immediately, without its release tail. Generations reject stale queued
  events. Queue overflow/backend failure stops listening and closes the gate.
  Key-up bypasses client/network locks; pending toggles cannot delay key release.
- In-app and global PTT are combined, so an unrelated in-app release does not
  cancel global PTT. Reconnect/settings changes clear held gates. The in-app
  custom binding is ignored when global PTT is configured; Hold to talk remains.
- Tray mute/deafen checkboxes track the active call. Diagnostics keep only ten
  semantic events in memory, with no raw typed text, disk log or telemetry.

Validation: native action/repeat/force-release and backwards-compatible binding
serialization tests; desktop conflict/default/history and combined-PTT immediate
release integration tests; Vitest capture drafts, unsupported backends, portal
choices and password/capture suspension. Packaged macOS smoke: open settings,
read real Caps, save no bindings, and confirm Not listening without a permission
prompt. Screenshot inspected. No Input Monitoring permission was granted by the
agent; actual background-game input is not claimed as tested.

## Manual acceptance matrix

| Environment | Recorded result | Remaining check |
|---|---|---|
| macOS | No-bindings startup/settings/save pass, no permission prompt | Grant Input Monitoring, capture keyboard/mouse, test PTT/toggles in another app/game, secure input and revoke/retry |
| Windows | Not run | Raw Input keyboard/mouse, game focus, UIPI restrictions, tray state and release |
| X11 | Not run | XI2 keyboard/mouse, focus, capture cancel and release |
| KDE Wayland | Not run | Portal binding UI, system-owned keys, PTT release and toggle callbacks |
| GNOME Wayland | Not run | Portal/version support, permission/configuration and toggle callbacks |
| Hyprland | Not run | Portal support and configured pass-through bindn behavior |
| Sway without portal | Not run | Explicit unavailable state; only opt-in evdev with user-provisioned permissions |

On every platform: clear bindings and restart (no listener); bind the three
separate actions; join voice; hold/release while gaming; mute/deafen from key and
tray; enter an app password field; capture/cancel while PTT was held; revoke
permission; disconnect/reconnect; verify no stuck transmit and record build,
backend, observations and pass-through behavior. Keep in-progress until this
matrix is completed. T-076 can now collect this evidence from installed builds.
