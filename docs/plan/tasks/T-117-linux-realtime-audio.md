# T-117 — Real-time voice priority on Linux through rtkit

Status: todo
Phase: 3
Size: S
Depends on: T-083, T-106
Paths: `crates/ss-audio/Cargo.toml`, `crates/ss-client-core/src/voice/`, `Cargo.toml`, `deny.toml`, `scripts/install-linux-deps.sh`, `docs/dependency-maintenance.md`

## Goal
The voice thread runs at real-time priority on Linux, as it already does on
macOS and Windows.

Source: T-106. The maintainer accepted rtkit over D-Bus on 2026-11-12.
T-083 added `audio_thread_priority`; on Linux promotion needs its
`with_dbus` feature, so the voice worker runs at normal priority there today.

## Scope
- Enable `audio_thread_priority`'s D-Bus path on Linux only, and check what
  it pulls in (`dbus`, `libdbus-sys`) against `deny.toml` and the
  dependency policy. Record the licences.
- Add the build dependency (`libdbus-1-dev` or the distribution's
  equivalent) to `scripts/install-linux-deps.sh` and the Linux CI job.
- When rtkit is missing or refuses (no session bus, a container, limits
  too low), keep running at normal priority and say so once in the log and
  in diagnostics, as T-083 does for other failures.

## Out of scope
macOS and Windows priority. Changing the audio callback threads.

## Acceptance criteria
- `cargo deny --all-features check` passes with the new dependencies.
- The Linux CI build compiles the D-Bus path.
- Manual on Linux: during a call, `chrt -p` on the voice thread shows a
  real-time policy, and with rtkit stopped the call still works and the log
  says why.

## Notes from implementation
