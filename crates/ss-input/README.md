# ss-input

Push-to-talk bindings that still reach the focused game. One backend per
operating system: Raw Input on Windows, a listen-only event tap on macOS,
XInput2 on X11, the GlobalShortcuts portal on Wayland, and an opt-in evdev
fallback.

The binding model and the press/release state machine are
platform-independent. Backends only report key and button edges.

## May depend on

`ss-core`, plus the platform crates for each backend.

## Must not depend on

`ss-proto`, `ss-net`, `ss-audio`, `ss-client-core`, `ss-server`, `ss-cli`.
No audio and no network.
