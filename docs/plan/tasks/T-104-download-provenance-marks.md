# T-104 — Mark saved downloads as coming from the internet

Status: awaiting-acceptance
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

Implemented 2026-11-11. `attachments::save_file` marks each file right after
`create_new` and before the first byte is written, so no complete unmarked
copy exists; if the mark fails the file is removed and the save returns
"Could not mark the file as downloaded: …".

**macOS: per-file `com.apple.quarantine`, not `LSFileQuarantineEnabled`.**
- The key quarantines every file the process creates: `sessions.json`, the
  stores, logs, exported reports and the files `tauri-plugin-updater`
  downloads and unpacks. Quarantined update files risk a Gatekeeper prompt
  or App Translocation after an update.
- `Info.plist` applies only to the bundle, so `cargo test` and `tauri dev`
  could not show it working. The per-file mark is tested on the code path
  that runs.
- `Info.plist` is unchanged.

The value is written with `fsetxattr` on the open file through `xattr` 1.7.0,
which was already in `Cargo.lock` via `tar`. The lockfile gains one line, the
dependency entry. The value is `0083;<seconds, 8 lowercase hex>;Sideband;<id>`.
A browser download on this Mac reads `0083;683ddeee;Brave;`, and apps
installed from Brave show the same format with an uppercase UUID after
Gatekeeper set the approval bits (`01c1`, `03c1`). The id is a fresh ULID
printed as a UUID. No Launch Services event record is written, so the
system's QuarantineEventsV2 history does not list Sideband downloads.
Foundation's
`URLResourceValues.quarantineProperties` parses the value: agent name, time
stamp and event id, owned by the current user.

Test written first: `attachments::tests::saved_downloads_carry_the_quarantine_mark`
saves a `.command` through `save_file` and reads the attribute back. Before
the change it failed with "a saved download has com.apple.quarantine".

**Windows: `Zone.Identifier` stream, `[ZoneTransfer]` / `ZoneId=3` (CRLF).**
It is written by path (`<file>:Zone.Identifier`) with `std::fs::write`. No
`ReferrerUrl` or `HostUrl`, so no server address lands on disk. This code
and its test, `saved_downloads_carry_the_mark_of_the_web`, are **not
compiled**: only `aarch64-apple-darwin` is installed here. A Downloads folder
on a volume without alternate data streams (FAT32, exFAT) now makes saving
fail instead of leaving an unmarked file.

**Linux:** there is no mark that the desktop checks before running a file.
`mark_downloaded` does nothing there. A saved file is not executable, since
`create_new` uses mode 0666 minus the umask.

Found while planning the manual checks: the server refuses `.exe`, `.msi`,
`.dll` and Mach-O content (`storage::DENIED_EXT`, `DENIED_MIME`), and
`UploadPolicy::executable_allow` is not exposed in the server config. A
received `.exe` therefore cannot be tested directly; a `.zip` that holds one
can.

Files outside the Paths: `Cargo.lock` (one line), `docs/security.md`
(section 12 and the T-104 row in section 13), `docs/plan/tasks/INDEX.md`,
`docs/plan/tasks/ACCEPTANCE.md`.

Manual checks (in `ACCEPTANCE.md`):
- macOS, installed build: save a received `.command` and a zipped `.app`
  (unzipped with Archive Utility). Opening each shows the Gatekeeper prompt,
  and `xattr -p com.apple.quarantine` shows `0083;…;Sideband;…`.
- Windows: dispatch `ci.yml` so the Mark of the Web code compiles and its
  test passes. Then, in an installed build, save a received `.zip` holding
  an `.exe` and a `.bat`. The `.exe` extracted with Explorer shows
  SmartScreen or the "downloaded from the internet" warning, and so does the
  `.bat`. File Properties shows "This file came from another computer".
