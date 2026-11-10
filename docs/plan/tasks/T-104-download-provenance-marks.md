# T-104 — Mark saved downloads as coming from the internet

Status: todo
Phase: 3
Size: S
Depends on: T-095, T-096
Paths: `apps/desktop/src-tauri/src/attachments.rs`, `apps/desktop/src-tauri/Cargo.toml`, `apps/desktop/src-tauri/Info.plist`

## Goal
A file another user sent gets the same operating-system warnings as one
from a browser before it is opened.

Source: T-095, confirmed by reading: `attachments::save_file` writes to
`Downloads/Sideband downloads` with no `com.apple.quarantine` attribute on
macOS and no `Zone.Identifier` stream on Windows, and `Info.plist` does not
set `LSFileQuarantineEnabled`. Gatekeeper and SmartScreen therefore never
check an executable received in chat.

## Scope
- macOS: set `com.apple.quarantine` on each saved file (for example with
  the `xattr` crate already in `Cargo.lock`), or set
  `LSFileQuarantineEnabled` and accept that every file the app writes is
  quarantined. Decide and record which.
- Windows: write a `Zone.Identifier` alternate data stream with
  `ZoneId=3` (Mark of the Web).
- Linux has no equivalent; note it.

## Out of scope
Server-side content checks.

## Acceptance criteria
- A test on macOS reads the attribute back from a saved file.
- Manual check on an installed build: opening a received `.app` or
  `.command` shows the Gatekeeper prompt; on Windows a received `.exe`
  shows SmartScreen or the "downloaded from the internet" warning.

## Notes from implementation
