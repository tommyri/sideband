# Engineering conventions

These apply to every task. Sub-agents: read before writing code.

## Rust

- Edition 2024, stable toolchain pinned in `rust-toolchain.toml`. MSRV = pinned.
- Workspace-level lints in root `Cargo.toml`: `clippy::all`, `clippy::pedantic`
  (allow a documented short list), `unsafe_code = "deny"` everywhere except
  `ss-input`, the platform modules of `ss-audio`, and its private
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
  lines saying what it owns and what it must not depend on.
- Feature flags for optional native pieces: `ss-audio` features
  `aec` (sonora), `rnnoise`, `deepfilter`, `dred`, `macos-voiceio`.

## Testing

- Unit tests next to code. Integration tests in `crates/*/tests/`.
- `ss-server` integration tests spin up the server in-process on an ephemeral
  port with a temp SQLite file and drive it with `ss-client-core`.
- Protocol codecs and the datagram header get `proptest` round-trip tests and
  a `cargo-fuzz` target (phase 4 wires it into CI).
- Audio DSP (jitter buffer, resampler glue, mixer) is tested with synthetic
  signals: known sine → assert on RMS/THD/latency, not on exact samples.
- `just check` = `cargo fmt --check && cargo clippy --all-targets -- -D
  warnings && cargo test --workspace`. CI runs it on ubuntu, macos, windows.
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
  by Tauri events; components never call Tauri directly—go through
  `src/lib/api.ts`.
- No network access from the webview except through the Tauri-provided
  `fetch` wrapper in `src/lib/http.ts` (honors cert pinning and auth).
- Accessibility: keyboard navigable, focus-visible, ARIA roles on custom
  widgets. Respect `prefers-reduced-motion`.
- `pnpm` as package manager. `pnpm check` = `tsc --noEmit && eslint && vitest
  run`.

## Git

- Branch `task/T-xxx-short-name`. Conventional commit messages
  (`feat(server): …`, `fix(audio): …`). One task per PR. PR description:
  what, why, how tested, deviations from the task file.
- Update the task file `Status:` and add "Notes from implementation" when done.

## Platform notes

- Windows: build with the MSVC toolchain; `windows` crate for Win32; don't
  pull `winapi`.
- macOS: `objc2` family crates; minimum macOS 13.
- Linux: minimum glibc 2.31 (Ubuntu 20.04); PipeWire ≥ 0.3.x headers at build
  time for the `pipewire` feature; build the server as `x86_64-unknown-linux-musl`.
