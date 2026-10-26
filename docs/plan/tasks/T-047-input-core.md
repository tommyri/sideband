# T-047 — `ss-input`: cross-platform PTT core (API, state machine, binding model)

Status: todo
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
