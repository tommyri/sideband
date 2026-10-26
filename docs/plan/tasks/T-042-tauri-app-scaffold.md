# T-042 — Tauri 2 app scaffold and Rust↔UI bridge

Status: todo
Phase: 3
Size: M
Depends on: T-040
Paths: `apps/desktop/`

## Goal
A running desktop app shell: React UI, Tauri commands/events bound to
`ss-client-core`, tray, single instance, window state, dev workflow.

## Scope
- `pnpm create tauri-app` equivalent: Vite + React 19 + TS strict + Tailwind
  + Zustand + `@tauri-apps/api`. ESLint/Prettier configs; `pnpm check`.
- `src-tauri`: `AppState { ClientSet, Storage }`; commands for every
  `ss-client-core` command (generated via a small macro or hand-written,
  returning `Result<T, ApiError>` serialized consistently); `ClientEvent`s
  forwarded as Tauri events `ss://event` with a tagged payload.
- Generate TypeScript types for commands, results and events from the Rust
  types (`specta` + `tauri-specta`, or `ts-rs`) so the UI never hand-writes
  protocol shapes; generated file checked in and verified in CI.
- `src/lib/api.ts` typed wrappers; `src/lib/events.ts` subscribes once and
  dispatches into Zustand stores; `src/lib/http.ts` fetch via Tauri (pinned
  cert, bearer).
- Plugins: `tauri-plugin-single-instance`, `tauri-plugin-window-state`,
  `tauri-plugin-store` (settings), tray icon with show/quit, close-to-tray
  option.
- Capabilities file minimal; CSP strict; no remote content except
  attachments/preview images from the connected server (dynamic CSP update
  or `asset:` proxying through Tauri).
- `just dev-desktop`, `just build-desktop`.
- Design tokens: a compact dark theme by default, light theme supported;
  font stack system UI; layout shell (server rail · channel sidebar · main ·
  member list) with placeholder panes.

## Out of scope
Feature UIs (T-043+), packaging (T-057).

## Acceptance criteria
- App starts on all three OSes in dev; connects to a local server via a
  temporary debug form; events reach the store (visible in a debug panel).
- `pnpm check` and `cargo clippy` clean.

## Notes from implementation
