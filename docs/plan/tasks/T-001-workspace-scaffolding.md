# T-001 — Workspace scaffolding and CI

Status: done
Phase: 0
Size: S
Depends on: —
Paths: `/Cargo.toml`, `/rust-toolchain.toml`, `/justfile`, `/crates/*`, `/.github/workflows/`

## Goal
A buildable Cargo workspace with empty crates in the layout from
`01-architecture.md`, shared lints/deps, a `justfile`, and CI that runs
`just check` on Linux, macOS and Windows.

## Scope
- Root `Cargo.toml` with `[workspace]`, `[workspace.package]` (edition 2024,
  license, repository), `[workspace.dependencies]` for: tokio, quinn, rustls,
  prost, prost-build, bytes, thiserror, anyhow, tracing, tracing-subscriber,
  serde, serde_json, sqlx (sqlite, runtime-tokio, migrate), axum, ulid,
  bitflags, parking_lot, rtrb, proptest.
- `[workspace.lints]` per `04-conventions.md`; each crate opts in with
  `lints.workspace = true`.
- Crates: `ss-core`, `ss-proto`, `ss-net`, `ss-audio`, `ss-input`,
  `ss-client-core`, `ss-cli`, `ss-server`, each with `lib.rs`/`main.rs`, a
  short `README.md`, and one trivial test.
- `apps/desktop/` placeholder directory with a README (Tauri scaffold is T-042).
- `justfile`: `check`, `test`, `fmt`, `clippy`, `run-server`, `run-cli`.
- `.github/workflows/ci.yml`: matrix ubuntu/macos/windows, cache, `just check`.
  Linux job installs `libasound2-dev libpipewire-0.3-dev libclang-dev`.
- `.gitignore`, `.editorconfig`, `rustfmt.toml` (default + `imports_granularity="Crate"`),
  `deny.toml` for `cargo-deny` (licenses: permissive only; advisories).
- `git init` if the repo is not initialized; initial commit.

## Out of scope
Any real code; Tauri; Docker.

## Acceptance criteria
- `just check` passes locally and in CI on all three OSes.
- `cargo deny check` passes.
- Every crate's README states its responsibility and allowed dependencies.

## Notes / references
- Keep the dependency graph direction from `01-architecture.md`; add a CI step
  `cargo tree -e no-dev -p ss-audio | grep -q ss-proto && exit 1`-style guard,
  or use `cargo-deny` bans, to prevent cycles.

## Notes from implementation

- Toolchain is pinned to 1.99.1. `sqlx` 0.9 requires rustc 1.94, and 1.99.1 was
  the current stable when this landed. The previously installed toolchain was
  1.93.1.
- `rustls` is held at 0.23. 0.24 is still a pre-release, and `quinn` 0.11
  uses 0.23.
- No `repository` field yet. There is no git remote.
- `imports_granularity` is still unstable, so `just fmt` runs nightly
  `rustfmt`. Compilation, clippy, and tests stay on 1.99.1. CI installs the
  nightly formatter before `just check`.
- Third-party crates are declared in `[workspace.dependencies]` and were
  checked to resolve, but no crate depends on them yet, so they are not in
  `Cargo.lock`. The task that first uses one adds it with `dep.workspace = true`.
- In-workspace crates are path dependencies with `version = "0.1.0"`.
  `cargo-deny` treats a path dependency without a version as a wildcard,
  which `deny.toml` forbids.
- This is the repository's first commit, so it landed on `main` rather than
  `task/T-001-workspace-scaffolding`.
