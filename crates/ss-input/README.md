# ss-input

Push-to-talk bindings that still reach the focused game. Decision D8: one
raw listener per operating system (Windows Raw Input, a listen-only macOS
event tap, X11 XInput2, the Wayland GlobalShortcuts portal, opt-in evdev),
not a hotkey registration API.

The binding model, capture gestures, and the press/release state machine
are platform-independent. Backends only report key and button edges.
`Backend::detect` returns `MockListener` until the operating-system
listeners land. Those listeners are not cargo features, so this crate
builds on Windows, macOS, and Linux as it stands.

## May depend on

`ss-core`, plus the platform crates each backend needs.

## Must not depend on

`ss-proto`, `ss-net`, `ss-audio`, `ss-client-core`, `ss-server`, `ss-cli`.
No audio and no network.
