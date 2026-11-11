# T-114 — Rebuild the server when a migration is added

Status: todo
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
