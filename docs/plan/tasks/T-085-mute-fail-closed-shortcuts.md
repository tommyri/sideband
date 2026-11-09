# T-085 — Mute fails closed; global shortcuts are never dropped

Status: todo
Phase: 3
Size: M
Depends on: T-041, T-058
Paths: `crates/ss-client-core/src/voice/mod.rs`, `apps/desktop/src-tauri/src/input.rs`, `apps/desktop/src-tauri/src/voice.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src-tauri/src/chat.rs`, `apps/desktop/src-tauri/src/dm.rs`

## Goal
When the user mutes, the microphone stops immediately, whatever the network
or the rest of the app is doing.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
reproduced.

## Scope
- `VoiceSession::set_self_mute` and `set_self_deaf` await
  `set_voice_state` first and store the local flag only on success. On
  `Disconnected` or timeout they return early with the gate open, and in the
  normal case the microphone stays live for the round trip. Store the local
  flag first when muting or deafening, then tell the server; when unmuting,
  keep the current order. Reconcile after reconnect.
- `toggle_guarded` (global hotkey and tray) uses `operations.try_lock()`
  and returns when it fails; the only trace is "Toggle skipped…" in a
  ten-entry in-memory log. `connect` holds `operations` for the whole
  handshake. Queue the toggle or apply the local gate without that lock.
- A global PTT press uses `clients.try_lock()` and treats failure as "not
  connected", so a press during any send, mark-read or history call is
  dropped until the key is released and pressed again.
- Root cause: most commands hold `state.clients` across network awaits
  (`load_history`, chat, DM, admin and webhook commands). Clone the client
  handle out of the map and drop the lock before awaiting, as `search.rs`
  and `server_settings.rs` already do. One stalled server must not block
  commands for another.
- The in-app PTT button sends press and release as two unordered async
  invokes; a fast tap could apply them out of order (reported, not
  confirmed). Sequence them.
- The PTT arming logic lives inside a `tokio::select!` body that rustfmt
  does not format. Move it into a function with unit tests.

## Out of scope
Moving shell logic into `ss-client-core` in general (T-095).

## Acceptance criteria
- Test: mute while the client is reconnecting; no audio frame is produced
  after the call returns, and the server state is corrected on reconnect.
- Test: a toggle issued while `connect` to another server is in flight
  takes effect.
- Test: a PTT press during a slow history request opens the gate.
- No command holds the client-map lock across a network await (grep or a
  lint-style test over the command modules).

## Notes from implementation
