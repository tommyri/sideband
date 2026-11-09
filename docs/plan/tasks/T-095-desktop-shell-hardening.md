# T-095 — Desktop shell hardening: capabilities, lints, locks, scaffold leftovers

Status: todo
Phase: 3
Size: M
Depends on: T-042, T-085
Paths: `apps/desktop/src-tauri/capabilities/default.json`, `apps/desktop/src-tauri/Cargo.toml`, `apps/desktop/src-tauri/src/`, `apps/desktop/package.json`, `apps/desktop/src/components/`, `apps/desktop/src/store/app-store.ts`, `apps/desktop/src/lib/`

## Goal
The shell meets the same rules as the crates, the webview holds no more
permissions than it uses, and installed builds contain no scaffold UI.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
exercised at runtime. Touches the command modules T-085 changes; run after.

## Scope
- `capabilities/default.json` grants `store:default` and
  `window-state:default`. The store plugin resolves `load(path)` without a
  scope, so script in the webview could load `sessions.json` and read every
  bearer token, or write any user-writable JSON file. The frontend never
  imports the plugin; only Rust uses `StoreExt`, which needs no capability.
  Remove both permissions and the `@tauri-apps/plugin-store` dependency,
  and confirm window-state restore still works from Rust.
- `apps/desktop/src-tauri/Cargo.toml` has no `[lints] workspace = true`,
  unlike every `crates/*` manifest, so about 8,000 lines skip
  `unsafe_code = "deny"` and `clippy::pedantic`. Add it and fix the
  fallout.
- `.lock().unwrap()` on std mutexes throughout `persistence.rs`,
  `notifications.rs`, `stream.rs` and `viewer.rs`: one panic poisons the
  lock and every later command panics. `persistence.rs` writes and fsyncs
  under that lock on runtime threads, on every per-user volume or mute
  change; `attachments.rs` writes up to 25 MiB synchronously in an async
  command; `testing.rs` has an `expect`. Use poison-tolerant locks as
  `input.rs` does, and `spawn_blocking` for file I/O.
- Scaffold leftovers: `debugOpen: true` opens the "Event inspector" at
  startup and its only toggle is not rendered under Tauri;
  `DebugPanel.tsx` shows a hard-coded "Bridge healthy · Live" and has Copy
  and Clear buttons with no handlers; `ServerRail.tsx` renders the first
  server as a fixed glyph and the avatar as hard-coded initials. Remove or
  gate behind a development flag, and wire real data.
- Conventions say components reach Tauri only through `src/lib/api.ts`.
  Ten components call `invokeBridge` directly and `AudioSettings.tsx` calls
  `listen` directly; `lib/http.ts` is one unused wrapper. Either route them
  through the API modules or amend `04-conventions.md` to the per-feature
  `api.ts` pattern that the code actually uses. There is no webview
  `fetch`, XHR or WebSocket anywhere; keep it that way with a lint rule.
- `01-architecture.md` says no business logic in `src-tauri`. Notification
  policy, moderator-move following, permission evaluation for
  `can_kick`/`can_ban`/`can_move`, unread derivation (twice) and the PTT
  arming state machine live there. List what should move to
  `ss-client-core`, move what is cheap, and open tasks for the rest.
- Unconfirmed, check on a Windows test build: `tauri.test.conf.json`
  replaces the `windows` array and may lose `dragDropEnabled: false`;
  middle-click on a chat link may open it in a webview popup; downloads are
  not quarantined on macOS.

## Out of scope
Chat behaviour (T-096). Voice command locking (T-085).

## Acceptance criteria
- A test or script asserts the capability file contains only permissions
  the frontend uses.
- `cargo clippy -p socialspeak-desktop --all-targets -- -D warnings`
  passes with workspace lints enabled.
- No `.lock().unwrap()` remains on a command path.
- A packaged build shows no event inspector and no placeholder identity.

## Notes from implementation
