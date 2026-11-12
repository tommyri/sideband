# T-114 — Rebuild the server when a migration is added

Status: done
Phase: 4
Size: S
Depends on: T-109
Paths: `crates/ss-server/build.rs`, `crates/ss-server/Cargo.toml`

## Goal
An incremental build always embeds every migration file.

Source: T-109. `sqlx::migrate!` embeds `migrations/` at compile time, and
`ss-server` has no build script, so adding a migration does not rebuild the
crate until a file under `src/` changes. A local build can then run without
the new migration while a clean build has it.

## Scope
- Add a build script that tells Cargo to rerun when `migrations/` changes
  (`cargo:rerun-if-changed=migrations`), as the sqlx documentation
  recommends. Check the current sqlx docs for the exact form.

## Out of scope
Migration content.

## Acceptance criteria
- After a build, adding an empty migration file and running `cargo build -p
  ss-server` recompiles `ss-server`; record the check in the notes.

## Notes from implementation

Done on main by the maintainer's session (2026-11-12). `crates/ss-server/build.rs`
prints `cargo:rerun-if-changed=migrations`, the form the sqlx `migrate!` docs
give for stable Rust (`sqlx migrate build-script` generates the same file).
Check: after a build, `cargo build -p ss-server -v` reported `Fresh ss-server`;
after `touch crates/ss-server/migrations/9999_t114_probe.sql` it reported
`Dirty ss-server … the file crates/ss-server/migrations has changed` and
recompiled. The probe file was then removed. The nightly
`--cfg sqlx_macros_unstable` alternative was not used: it needs nightly for
every build and recompiles the whole dependency tree once.
