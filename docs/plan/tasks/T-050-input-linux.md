# T-050 — `ss-input` Linux backends (X11 XInput2, Wayland GlobalShortcuts portal, evdev opt-in)

Status: in-progress
Phase: 3
Size: M
Depends on: T-047
Paths: `crates/ss-input/src/linux/{x11.rs,portal.rs,evdev.rs,mod.rs}`

## Goal
Best-possible PTT on Linux with honest fallbacks, per the decision in
`00-overview.md` D8.

## Scope
- Detection order: `WAYLAND_DISPLAY` set → portal (then evdev if user opted
  in) → else X11.
- **X11**: `x11rb` XInput2 raw events (`XI_RawKeyPress/Release`,
  `XI_RawButtonPress/Release`) selected on the root window; keycodes via
  `xkbcommon` to `Code`; passive, passes through, supports mouse buttons.
- **Wayland portal**: `ashpd` `GlobalShortcuts`: create session, `BindShortcuts`
  with ids `push-to-talk`, `toggle-mute`, `toggle-deafen` and preferred
  triggers; map `Activated`/`Deactivated` to press/release; `ListShortcuts`
  to display the user's actual binding; `ConfigureShortcuts` (portal v2) to
  open system settings; handle session loss and rebind. `Caps`:
  `binding_set_by_system = true`, `mouse_buttons = false`, pass-through
  compositor-dependent (document per compositor: KDE/GNOME consume, Hyprland
  `bindn` passes).
- **evdev** (explicit opt-in setting, off by default): read `/dev/input/event*`
  via `evdev` crate with `inotify` hot-plug; requires `input` group; shows a
  warning in UI about the security implication; supports everything.
- Unsupported (Sway/wlroots/COSMIC without portal): return `Caps::none()`
  so the UI defaults to voice activation with an explanation.

## Out of scope
Custom compositor protocols (Hyprland-specific), Flatpak portal nuances
beyond the above.

## Acceptance criteria
- Manual on: X11 session, KDE Wayland, GNOME 48+ Wayland, Hyprland, Sway
  (expect graceful "unavailable"). Results recorded in task notes.
- Integration test for the portal client against a mocked D-Bus service
  (`zbus` test harness) covering bind → activate → deactivate → session lost
  → rebind.

## Notes from implementation

- Implemented native Linux detection and listeners. Wayland takes precedence
  over DISPLAY/XWayland; evdev is available only through explicit
  `Backend::detect_linux(Options { allow_evdev: true, .. })` opt-in when the
  portal is absent. Headless/unsupported sessions expose `Caps::none()`.
- XInput2 handles physical keyboard edges, side-specific modifiers and mouse
  buttons without grabs. Deviation: physical XKB names are queried directly
  through x11rb rather than xkbcommon; no text/keysym translation or native
  xkbcommon linkage is needed for HID bindings.
- GlobalShortcuts registers all three action IDs with preferred triggers,
  exposes actual binding descriptions and portal-v2 configuration through a
  cloned Settings handle, filters stale session signals, and releases/rebinds
  on Session.Closed. Service/bus loss releases and reports an error; restart
  the listener after the service returns. Mute/deafen use an explicit action
  callback because the existing Listener handler carries only PTT edges.
- evdev uses nonblocking synchronized reads, inotify hotplug, per-device
  held-key tracking, and release on removal. Capabilities carry the input
  group permission and whole-system keyboard-access warning for T-051.
- Validation on Linux ARM64 in an isolated Docker container: 31 unit tests
  and one doctest passed; the explicitly run mock D-Bus bind → activate →
  deactivate → configure → session loss → rebind test passed; the Xvfb
  keyboard/Mouse5/capture/stop test passed. Strict clippy passed on native
  Linux ARM64 and cross-target Linux x86_64. CI runs both integration tests.
- Dependency graph and offline advisory/license/source audit passed.
- Full macOS workspace `just check` passed (format, strict clippy, tests).
- Still in progress pending the required manual X11, KDE Wayland, GNOME 48+,
  Hyprland, Sway and authorized evdev-device acceptance. No desktop/game or
  physical evdev-device acceptance is claimed from the container tests.
  Setup, application API and manual checklist: `crates/ss-input/linux.md`.
