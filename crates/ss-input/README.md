# ss-input

Push-to-talk bindings that still reach the focused game. Decision D8: one
raw listener per operating system (Windows Raw Input, a listen-only macOS
event tap, X11 XInput2, the Wayland GlobalShortcuts portal, opt-in evdev),
not a hotkey registration API.

The binding model, capture gestures, and the press/release state machine
are platform-independent. Backends only report key and button edges.
`Backend::detect` selects the native macOS listener; Windows and Linux keep
`MockListener` pending their backends. No platform cargo features are needed.

macOS asks for Input Monitoring at start/capture, never during detection.
The tap uses its own thread and a bounded event ring. Revocation/secure input
force-release and surface through `Handle::status`; recheck capabilities on
app focus, then drop/restart the handle after access returns. Controls and
capture block, so keep them off the UI/async runtime thread. Handlers must
stay short and send controls from another thread.

Manual check: `cargo run -p ss-input --example macos_ptt` (F13/Mouse4), or add
`capture` to record a chord. See the T-049 task notes for the game/TCC checks.
TCC grants depend on signing identity: use a stable app identifier/path and
signing identity; rebuilds signed ad-hoc may need a new grant. Do not request
Accessibility or change TCC settings automatically.

## May depend on

`ss-core`, plus the platform crates each backend needs.

## Must not depend on

`ss-proto`, `ss-net`, `ss-audio`, `ss-client-core`, `ss-server`, `ss-cli`.
No audio and no network.
