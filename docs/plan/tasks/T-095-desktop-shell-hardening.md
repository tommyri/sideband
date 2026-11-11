# T-095 — Desktop shell hardening: capabilities, lints, locks, scaffold leftovers

Status: awaiting-acceptance
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

Implemented (2026-11-10).

Findings, checked by reading and by tests written first where testable:
- Capabilities: confirmed, and wider than reported. The frontend imports only
  `invoke` and `listen`, so `core:default` and the three `core:window:*`
  grants were unused too (Rust shows and focuses the window, which needs no
  capability). `default.json` now grants `core:event:allow-listen` and
  `allow-unlisten` only. `config_tests::capability_grants_only_what_the_frontend_imports`
  scans `src/` for `@tauri-apps/*` imports, maps each to its permissions and
  requires the file to match exactly; it failed on the old file. Window
  state is still restored: tauri-plugin-window-state 2.5.0 restores in its
  Rust `on_window_ready` hook, and its JS commands were never called.
  `@tauri-apps/plugin-store` is gone from `package.json` and, by hand, from
  `pnpm-lock.yaml`; Rust keeps `tauri-plugin-store` for `StoreExt`.
- Lints: confirmed. `[lints] workspace = true` raised 90 clippy errors and
  no `unsafe`. Fixed by refactoring: `viewer::run` (a select! body that
  rustfmt could not format) is now a `Playback` state type; the publisher
  thread is `stream::Publisher`; `lib::run` setup, the viewer menu,
  `onboarding::probe_with_policy` (QUIC trust and HTTPS metadata),
  `notifications::observe` (badge, cues, message alert) and
  `voice_view::snapshot` are split; casts and defaults are explicit.
- Locks: confirmed for `persistence`, `notifications`, `stream` and
  `viewer`. All std mutexes go through `state::lock`, which recovers a
  poisoned guard (the `input.rs` pattern). Partly refuted for persistence:
  `ss-client-core` already saves sessions, volumes and mutes on blocking
  workers. The real fault was that the data lock was held across the file
  write and fsync, so runtime-thread readers (the member list reads every
  volume, every client event reads notification preferences) waited on the
  disk. Writes now hold a separate `writer` lock; `data` is held only to copy
  or swap. The shell's own writes (label, remove, reorder, notification
  preferences) run through `DesktopStorage::write` on a blocking worker.
  Downloads are saved by `spawn_blocking` after the cancellable part;
  `test_build_identity` returns an error instead of `expect`. The seen-message
  dedupe no longer unwraps. Tests: a panic inside a write leaves storage
  usable and unchanged; reads succeed while a write holds `writer`.
- Scaffold: confirmed. The event inspector renders only under
  `import.meta.env.DEV`, starts closed, has a rail toggle in development
  (the old toggle existed only in the browser-preview mock), reports real
  bridge state and an event count, and Copy/Clear work. The rail shows
  initials for every server and the signed-in member of the selected server
  (display name and presence) instead of `TR`, or nothing before a server
  view arrives. `App.test.tsx` stubs `DEV` false and checks that no
  inspector, toggle or placeholder identity is rendered.
- API boundary: confirmed (ten components plus `listen` in
  `AudioSettings`). The calls moved into feature modules (`admin/moderation.ts`,
  `admin/serverSettingsApi.ts`, `updates/api.ts`, `search/api.ts`,
  `testing/api.ts`, `diagnostics/api.ts`, `notifications/api.ts`, and the
  audio and shortcut stores); `lib/http.ts` is deleted. `04-conventions.md`
  now describes the per-feature pattern and `01-architecture.md` no longer
  mentions a webview fetch. ESLint rejects `fetch`, XHR, WebSocket,
  EventSource, WebTransport and `sendBeacon` in `src/`, and `@tauri-apps/*`
  or `invokeBridge` imports in non-test `.tsx` files.
- Business logic. Moved to new `ss-client-core` modules: `unread` (one rule
  for channels, DMs and the rail badge, which used three slightly different
  ones; the rail now also counts newer cached messages and only DMs the user
  belongs to), `authority` (server permissions, role rank, kick/ban/mute/move
  rights used by `voice_view`), `ptt` (`PttArming`, unchanged, with its
  tests). Opened tasks for the rest: T-102 notification policy, T-103
  moderator-move following and seat reconciliation (needs `voice/`, owned by
  T-084), T-105 the other permission projections (`role_admin`,
  `stream::can_publish`, chat flags, `channel_admin`). The T-085 arming rule
  (a press for a call the tick has not seen needs a release first) is kept.
- `tauri.test.conf.json`: confirmed by reading tauri-utils 2.10.2, which
  merges `--config` files with `json_patch::merge` (RFC 7396: arrays are
  replaced). Test builds on every OS lost `dragDropEnabled: false`. Fixed;
  `config_tests::test_build_windows_keep_every_base_window_setting` failed
  before.
- Middle-click popup: refuted by reading wry 0.57.1. With no new-window
  handler, WebView2 marks `NewWindowRequested` handled (no window), WKWebView
  returns no web view and WebKitGTK has no `create` handler. A middle-click on
  a chat link therefore does nothing; opening it through `open_chat_link` is
  chat behaviour (T-096).
- macOS quarantine: confirmed by reading (no `com.apple.quarantine`, no
  `LSFileQuarantineEnabled`; Windows has no Mark of the Web either). Opened
  T-104 rather than add a dependency here.

Allows kept, each with a reason:
- `needless_pass_by_value` on the Tauri commands `open_chat_link`,
  `notification_preferences`, `test_notification`, `stop_screen_share`,
  `stream_viewers` (Tauri extracts arguments by value).
- `struct_excessive_bools` on the bridge structs `AudioEnvironment`,
  `input::Snapshot`, `RoleSnapshot`, `MemberView`, `ServerView` and on
  `ss_client_core::authority::ModerationRights`.
- `unused_async` on `chat_snapshot`, so Tauri runs it off the main thread.
- `cast_possible_truncation`/`cast_sign_loss` in `voice_view::percent`
  (clamped gain) and `cast_precision_loss` in `viewer_gpu::px` (pixel sizes).
- `too_many_lines` on eight end-to-end or fixture tests.

Files outside Paths: `apps/desktop/eslint.config.js`, `pnpm-lock.yaml`,
`src-tauri/tauri.test.conf.json`, `src-tauri/build.rs`,
`src-tauri/examples/viewer_smoke.rs`, `src/App.tsx`, `src/App.test.tsx`, the
ten components and new API modules under `src/features/`,
`crates/ss-client-core/src/{lib,authority,unread,ptt}.rs`,
`docs/plan/01-architecture.md`, `docs/plan/04-conventions.md`, the task
index and T-102 to T-105.

Manual checks:
- Run `pnpm install --frozen-lockfile` with the pinned pnpm to confirm the
  hand-edited lockfile.
- Installed app with the trimmed capability: live chat, speaking indicators,
  the audio level meter and notification activation still update (all use
  `listen`), and the console shows no ACL denials.
- Window size and position survive a restart.
- Windows test build: dropping a file on the composer attaches it (drag-drop
  now disabled at the native layer as in release builds); middle-clicking a
  chat link opens no window.
- Packaged build: no event inspector or toggle; the rail avatar shows your
  own initials.
