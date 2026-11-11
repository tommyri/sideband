# Engineering conventions

These apply to every task. Sub-agents: read before writing code.

## Rust

- Edition 2024, stable toolchain pinned in `rust-toolchain.toml`. MSRV = pinned.
- Workspace-level lints in root `Cargo.toml`: `clippy::all`, `clippy::pedantic`
  (allow a documented short list), `unsafe_code = "deny"` everywhere except
  `ss-input`, the platform modules of `ss-audio` (`platform` and the
  `device::macos_voiceio::raw` input-proc module, T-088), and its private
  `codec::native` Opus bridge, which locally allow it with a `// SAFETY:`
  comment on every block. The codec exception covers missing decoder CTLs,
  packet framing validation, and DRED offsets; native state never escapes.
- Errors: `thiserror` enums per crate boundary; `anyhow` only in binaries
  (`ss-server`, `ss-cli`, `src-tauri`). Never `unwrap()`/`expect()` outside
  tests except for invariants with a comment explaining why they hold.
- Logging: `tracing`. `info!` for lifecycle, `debug!` for per-request,
  `trace!` for per-packet. Never log inside audio callbacks or the datagram
  forwarding path (use counters).
- Async: `tokio`. No blocking calls on the runtime; use `spawn_blocking` for
  argon2, image processing, filesystem walks.
- Real-time audio code (anything called from a `cpal` callback): no
  allocation, no locks, no `await`, no logging, no syscalls. Communicate with
  `rtrb` ring buffers and atomics. Pre-allocate in setup.
- Shared deps live in `[workspace.dependencies]`; crates use
  `dep.workspace = true`.
- Public items documented with `///`. Each crate has a `README.md` of ≤ 40
  lines saying what it owns and what it must not depend on. Trim, do not
  raise the limit (T-099 trimmed `ss-audio`, `ss-client-core`, `ss-server`).
- Feature flags for optional native pieces: `ss-audio` features
  `aec` (sonora), `rnnoise`, `dred`, `macos-voiceio`. `deepfilter` is a
  reserved, empty feature: no DeepFilterNet3 build meets the dependency and
  model rules (T-053), so enabling it builds nothing. `ss-video` features
  `capture` and `software` (default), `render`, `pipewire`, `vaapi`,
  `nvidia` (the last three Linux-only).

## Testing

- Unit tests next to code. Integration tests in `crates/*/tests/`.
- `ss-server` integration tests spin up the server in-process on an ephemeral
  port with a temp SQLite file and drive it with `ss-client-core`.
- Protocol codecs and the datagram header get `proptest` round-trip tests and
  a `cargo-fuzz` target (phase 4 wires it into CI).
- Audio DSP (jitter buffer, resampler glue, mixer) is tested with synthetic
  signals: known sine → assert on RMS/THD/latency, not on exact samples.
- `just check` = `cargo fmt --check && cargo clippy --all-targets -- -D
  warnings && cargo test --workspace`, then `just plan-check` (Python, under
  a second). It is the routine gate, run locally
  before every commit (with `just check-desktop` for desktop or frontend
  changes). Every GitHub workflow runs only when dispatched by hand; nothing
  runs on push or pull request (maintainer decision, 2026-11-11, T-098).
  Dispatch CI (`ci.yml`: Ubuntu, macOS and Windows, container smoke,
  `check-deps`, `cargo deny`) before merging changes to platform code,
  features, dependencies, `deploy/` or workflows, and the public repository
  checks (`public-repo.yml`, Gitleaks) before publishing or releasing. The
  release workflows run CI for the commit they publish.
- Anything you write solely to verify your work goes under `/tmp` or is
  deleted before the PR.

## Protocol & schema changes

- `.proto` files: add fields, never renumber or reuse tags. Mark removed
  fields `reserved`.
- SQL migrations are append-only, numbered `NNNN_description.sql`, idempotent
  where possible. Never edit a merged migration.
- Any change to `02-protocol.md` or `03-data-model.md` is made in the same PR
  as the code and called out in the PR description.
- A change to encoder behavior, a bitrate, or a numbered decision updates
  `docs/design/` in that same PR. The wire table in `02-protocol.md` §4 and
  `VoiceProfile::encoder_params` stay in agreement with `docs/design/voice.md`.

## Frontend (apps/desktop/src)

- React 19 + TypeScript strict + Vite + Tailwind. State in Zustand stores fed
  by Tauri events. Components never call Tauri directly: shell-wide commands
  go through `src/lib/api.ts`, feature commands through that feature's
  `api.ts` (or its store/model module), and both use `invokeBridge` from
  `src/lib/tauri.ts`. ESLint rejects `@tauri-apps/*` and `invokeBridge`
  imports in non-test `.tsx` files.
- No network access from the webview: no `fetch`, XHR, WebSocket,
  EventSource or `sendBeacon` (ESLint rejects them). History, attachments,
  previews and uploads are Rust commands, which keep certificate pins and
  bearer tokens out of script.
- The webview capability (`src-tauri/capabilities/default.json`) grants only
  what the frontend imports; `config_tests.rs` fails when an import needs a
  permission the file does not grant, or the file grants one nothing uses.
  Rust-side plugin use needs no capability.
- Development-only UI (the event inspector) renders only under
  `import.meta.env.DEV`, so packaged builds do not contain it.
- Accessibility: keyboard navigable, focus-visible, ARIA roles on custom
  widgets. Respect `prefers-reduced-motion`.
- `pnpm` as package manager, at the exact `packageManager` version (another
  pnpm rewrites `pnpm-lock.yaml`). `pnpm check` = `tsc -b && eslint . &&
  prettier --check . && vitest run`; `just check-desktop` first stops unless
  Node and pnpm match `apps/desktop/package.json`.

## Git

- Branch `task/T-xxx-short-name`. Conventional commit messages
  (`feat(server): …`, `fix(audio): …`). One task per PR. PR description:
  what, why, how tested, deviations from the task file.
- Update the task file `Status:` and add "Notes from implementation" when done.

## Plan documents

- Status values: `todo`, `in-progress` (code or automated work in the task's
  own scope is still missing), `awaiting-acceptance` (merged; only hands-on
  checks on real hardware, a VPS or GitHub, or a maintainer decision remain;
  each one is a row in [`tasks/ACCEPTANCE.md`](tasks/ACCEPTANCE.md)), `done`,
  `blocked`. A task's row in `tasks/INDEX.md` and its `Status:` line agree.
- `tasks/INDEX.md`'s dependency graph and waves are generated from its task
  tables: after changing a row's "Depends on" or adding a task, run
  `just plan-index`. `just plan-check` (part of `just check`) fails when they
  are stale, a row disagrees with its task file, an `awaiting-acceptance`
  task has no row in `tasks/ACCEPTANCE.md`, or `02-protocol.md` and the
  routes and envelopes in the code name different things
  (`scripts/check-protocol-doc.py`).

## Platform notes

- Windows: build with the MSVC toolchain; `windows` crate for Win32; don't
  pull `winapi`.
- macOS: `objc2` family crates; minimum macOS 13.
- Linux: minimum glibc 2.31 (Ubuntu 20.04); PipeWire ≥ 0.3.x headers at build
  time for the `pipewire` feature; build the server as `x86_64-unknown-linux-musl`.
