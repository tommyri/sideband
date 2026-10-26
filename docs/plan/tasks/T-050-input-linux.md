# T-050 — `ss-input` Linux backends (X11 XInput2, Wayland GlobalShortcuts portal, evdev opt-in)

Status: todo
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
