# T-047 — `ss-input`: cross-platform PTT core (API, state machine, binding model)

Status: done
Phase: 3
Size: S
Depends on: T-003
Paths: `crates/ss-input/src/lib.rs`, `crates/ss-input/src/binding.rs`, `crates/ss-input/src/state.rs`

## Goal
Define the platform-independent half of global input so the per-OS backends
(T-048–T-050) plug into one API, and PTT behaves identically everywhere.

## Scope
- `Binding`: physical key (`Code`, USB HID-like, e.g. `KeyV`, `F13`) or mouse
  button (`Mouse4`, `Mouse5`, `Middle`), optional modifiers; serde; human
  label per OS.
- `Listener` trait: `start(handler) -> Result<Handle>`, `capabilities() ->
  Caps { mouse_buttons, passthrough, needs_permission: Option<Permission>,
  binding_set_by_system }`, `Handle::update_bindings(&[Binding])`, `stop()`.
- `Backend::detect()` picks the right backend at runtime (Windows hooks,
  macOS event tap, X11 XInput2, Wayland portal, evdev opt-in) and returns
  `Caps` so the UI can explain limitations.
- `PttStateMachine`: edge detection, debounce (coalesce release→press within
  30 ms), release delay, `force_release()` for focus-loss/sleep/tap-disabled,
  emits `PttEvent::{Pressed, Released}`.
- Binding capture mode: `capture_next() -> Binding` for the settings UI
  (respecting `binding_set_by_system` where the compositor owns bindings).
- A `MockListener` for tests and for the CLI.

## Out of scope
Actual OS hooks (T-048–T-050).

## Acceptance criteria
- State machine unit tests: debounce, release delay, stuck-key recovery,
  modifier-combo bindings.
- Crate compiles on all OSes with no backend enabled (mock only).

## Notes from implementation

- `Backend::detect` always returns `MockListener`. There is no backend cargo
  feature and no OS module. Windows, macOS, and Linux all compile this crate
  as it stands. Later tasks replace the body of `detect` (Raw Input,
  listen-only `CGEventTap`, X11 XInput2, the GlobalShortcuts portal, opt-in
  evdev).
- `capture_next(timeout)` returns `CapturedBinding`, not a bare `Binding`.
  `SetBySystem` is the compositor case: the default method returns it
  immediately and does not observe a key. `cancel_capture` unblocks a waiting
  capture. `CaptureSession` is the shared gesture recorder (modifiers, then
  the trigger; a modifier alone completes on release; mouse is ignored when
  `Caps.mouse_buttons` is false).
- `Handle` also has `set_release_delay` and `force_release`. The mock has no
  thread, so `MockListener::inject` and `poll` drive edges and the release
  timer for tests and the CLI.
- `Caps` includes `limitations: Vec<String>`, plus `Caps::none()` and
  `Caps::mock()`. `Error::PermissionRequired` carries the grant and an
  optional settings URL for the macOS and evdev backends.
- `Code` is the USB HID keyboard page through F24, not only F13, plus digits,
  navigation, numpad, and the two ISO keys. Discriminants are HID usage IDs.
  Usage `0x2A` is `Backspace`; forward delete is `0x4C`. Power (`0x66`) is
  omitted. Mouse buttons include Left and Right so capture can name them.
- The release wait is `max(release_delay, DEBOUNCE)` with `DEBOUNCE` at 30 ms,
  so a zero delay still coalesces chatter. A press at exactly 30 ms stays
  held. `DEFAULT_RELEASE_DELAY` is 150 ms, matching the voice-session default.
- `force_release` clears tracked keys and suppresses those that were down
  until the matching up, so a repeat cannot reopen transmit. A backend that
  sees the key is already up must inject that up.
- Mute and deafen shortcut roles are not on `Binding`. This state machine is
  push-to-talk only; T-050 and T-058 can add those actions.
- `Cargo.lock` gained serde, thiserror, and tracing because nothing in the
  workspace depended on them yet.
