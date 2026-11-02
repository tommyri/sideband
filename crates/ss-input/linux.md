# Linux input

`Backend::detect()` checks Wayland first, including when DISPLAY points to
XWayland. It probes the GlobalShortcuts version without creating a session or
showing consent. Missing portals report `Caps::none()`. Without Wayland it
selects X11 when DISPLAY is present; headless sessions report unavailable.

`Backend::detect_linux(linux::Options)` accepts an explicit `allow_evdev`
setting, false by default. Only a missing Wayland portal permits this fallback.
It never opens input devices during detection or bypasses declined portal
consent. The future settings UI must display the capability warning before
enabling it: input-group access can read every key in every application.
The library never changes groups, permissions, or compositor configuration.

X11 selects XInput2 raw keyboard and button events on each root window.
Physical XKB key names map to HID codes independently of the current layout.
The implementation queries XKB through x11rb directly instead of linking
xkbcommon: only physical names are needed, not text/keysym translation.
Wheel buttons are ignored; X11 buttons 8/9 map to Mouse4/5. No grabs are used.

evdev uses nonblocking synchronized device streams and inotify hotplug.
Device loss releases transmit; repeats are ignored, and a release from one
keyboard cannot release a key still held on another. SYN_DROPPED recovery
comes from the evdev synchronized stream.

The Wayland portal registers push-to-talk, toggle-mute and toggle-deafen.
Pass preferred XDG trigger strings and an action callback through
`linux::portal::Options`. Keep its cloned `Settings` to read the compositor's
actual binding descriptions and call `configure()` for portal v2.
On v1, configure returns an error without interrupting PTT. Capture returns
`SetBySystem`; `update_bindings` refuses to override system-owned bindings.
PTT still uses the shared debounce/release-delay state machine. Mute/deafen
callbacks run on the input thread and must remain short.

Closed sessions release and rebind. Portal service or bus loss releases and
surfaces through `Handle::status()`; drop/restart the listener after service
recovery. Stop closes the portal session. Start can wait for consent (up to
120 seconds), so call start/capture off the UI and async runtime threads.

Portal passthrough is reported conservatively as false. KDE/GNOME can consume
shortcuts; Hyprland pass-through requires compositor `bindn` configuration.
No compositor capability is inferred merely from its desktop name.

## Automated acceptance

```sh
cargo test -p ss-input
dbus-run-session -- cargo test -p ss-input bind_activate_deactivate_session_loss_and_rebind -- --ignored
xvfb-run -a cargo test -p ss-input x11_native_key_mouse_capture_and_stop -- --ignored
```

The latter commands require dbus, Xvfb and xdotool. CI runs both on Linux.
Manual desktop acceptance remains necessary: on X11, KDE Wayland, GNOME 48+
Wayland and Hyprland check press/release in a focused game, left/right
modifiers, Mouse5 where supported, capture cancellation, sleep and device
removal, and compositor settings changes. On Sway without a portal verify
unavailable capabilities and voice activation fallback. Test evdev only after
explicit opt-in and input-group authorization.
