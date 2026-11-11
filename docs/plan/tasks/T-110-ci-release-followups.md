# T-110 — CI, fuzzing and release follow-ups

Status: todo
Phase: 4
Size: S
Depends on: T-097, T-098
Paths: `.github/`, `fuzz/`, `deny.toml`, `Cargo.toml`

## Goal
What T-097, T-098 and T-101 left between them is owned by one task.

Source: T-099, checked against `0d0cb16`.

## Scope
- `fuzz.yml` still runs on `ubuntu-latest`; every other workflow pins
  `ubuntu-24.04`. T-098 left it to T-101, which did not change it.
- `fuzz/Cargo.lock` is stale against the workspace (T-101); refresh it and
  make `fuzz.yml` use `--locked`.
- `cargo deny check` fails on RUSTSEC-2026-0459 until `sqlx` allows
  `libsqlite3-sys` 0.39 (T-098). Upgrade when it does, or add a dated,
  scoped ignore with the reason, so `ci.yml` can pass.
- Windows signing still runs inside `tauri build`, where a build script can
  sign with the key (T-097). Build unsigned, then sign and rebundle in a
  separate job, or use a remote signing service. Until then
  `docs/security.md` §11 says so.
- BuildKit and `dtolnay/rust-toolchain` are not pinned by Dependabot
  (T-097).

## Out of scope
Push triggers: CI stays manual-only (maintainer decision, T-098).

## Acceptance criteria
- A dispatched `ci.yml` passes `cargo deny`, and `fuzz.yml` runs every
  target on `ubuntu-24.04` with `--locked`.
- The Windows release job that holds the signing key runs no dependency
  build scripts, or the remaining exposure is recorded in
  `docs/security.md`.

## Notes from implementation
