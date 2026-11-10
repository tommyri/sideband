# T-103 — Voice membership reconciliation moves to ss-client-core

Status: todo
Phase: 3
Size: M
Depends on: T-084, T-089, T-095
Paths: `crates/ss-client-core/src/voice/`, `apps/desktop/src-tauri/src/voice.rs`

## Goal
Following a moderator move, dropping a call whose seat is gone and pausing
PTT while reconnecting behave the same in every host and are tested
without Tauri.

Source: T-095. `voice::observe` in the shell runs every 40 ms and decides
when the seat moved (restart the session muted in the new channel with its
profile), when it is gone (end the call) and when the connection is
retrying (release PTT). It needs `VoiceSession`, which T-084 owns, so T-095
did not move it.

## Scope
- Give `VoiceSession` (or a small owner type next to it) the reconcile
  step: it reads the call's own `StateStore`, tracks the connection id across
  a non-resumed reconnect, and reports `Follow { channel }`, `Ended` or
  `Retrying`, or performs the follow itself and reports it.
- The shell keeps the watch timer, the `ss://voice-session` and
  `ss://speaking` events, and the `operations` serialization.

## Out of scope
Changing the follow-muted rule. Voice command locking (T-085).

## Acceptance criteria
- An `ss-server` integration test moves a member and sees the client
  follow muted into the new channel without the desktop crate.
- `voice::observe` in the shell has no membership branches.

## Notes from implementation
