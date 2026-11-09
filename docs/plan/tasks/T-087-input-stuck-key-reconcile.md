# T-087 — Push-to-talk cannot stick: XI 2.1 and physical-state reconcile

Status: todo
Phase: 3
Size: S
Depends on: T-049, T-050
Paths: `crates/ss-input/src/linux/x11.rs`, `crates/ss-input/src/linux/mod.rs`, `crates/ss-input/src/macos/mod.rs`, `crates/ss-input/src/windows/`

## Goal
A missed key-up never leaves the microphone open.

Source: 2026-11-09 review at `d861a0a`. Read-only analysis; the Linux and
Windows code cannot be compiled on the development Mac. Needs a Linux
machine, and a Windows machine for the last bullet.

## Scope
- The X11 backend negotiates XInput 2.0. Per the XI2 protocol, 2.0 clients
  receive raw events only while the device is ungrabbed; 2.1 and later
  always receive them. A Mouse4/5 press over a window that selects
  ButtonPress starts an implicit grab and the raw release is filtered;
  keyboard grabs (fullscreen SDL games, menus, lockers) drop both edges.
  Call `xi_query_version(2, 2)`, require a reply of at least 2.1 and report
  the backend as unavailable otherwise.
- X11 has no physical-state guard. Add a periodic reconcile against
  `XIQueryPointer`/`QueryKeymap`, like the Windows `StuckGuard`, that
  releases a held binding whose key is physically up.
- macOS reconciles only on tap-disable or overflow. Add the same periodic
  check so a key-up lost across sleep with no lock screen is released.
- Windows, reported and not confirmed: a key swallowed by AutoHotkey or
  PowerToys still reaches Raw Input while `GetAsyncKeyState` reports it up,
  so `StuckGuard` would synthesise a release every 250 ms and PTT would
  flap. Raw-input registration is per process and tao/winit may register
  too; `RIDEV_REMOVE` on drop removes it for the process. Test both in the
  packaged app and fix what reproduces.

## Out of scope
Wayland portal behaviour (T-050 acceptance matrix).

## Acceptance criteria
- Xvfb test with a client window that selects ButtonPress and one that
  grabs the keyboard: press and release are both seen. Today's test clicks
  on a bare root window and cannot detect the bug.
- Unit test for the reconcile: held binding plus physical-up state yields
  one release.
- Rows added to the T-058 manual matrix for grab scenarios and sleep.

## Notes from implementation
