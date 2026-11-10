# T-085 — Mute fails closed; global shortcuts are never dropped

Status: in-progress
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

Implemented (2026-11-10):

Findings confirmed by failing tests written first:
- `set_self_mute` with the old code: the gate stayed open for the whole
  round trip, and while reconnecting the call returned `Disconnected` with
  the gate open. `set_self_state` with an `operations.try_lock()` skip (the
  old toggle path) failed while `operations` was held. The fast-tap in-app
  PTT race was confirmed only by a Vitest that showed release was sent before
  press settled; it was not reproduced in the app.
- Root cause of dropped shortcuts: `state.clients` was a tokio mutex held
  across network awaits in history, chat, DM, admin, webhook, audit,
  moderation, role and channel admin commands, voice join/stop, the DND
  presence loop and the voice watcher. The watcher also held the voice lock
  across `VoiceSession::start` when following a moderator move.

Changes:
- Core: `set_self_mute`/`set_self_deaf` close the gate, record the request
  and queue `SetVoiceState` synchronously, then return a `'static` future.
  Unmuting/undeafening opens the gate only on the server's Ack, and only if no
  newer request was made (revision). A failed request leaves the gate closed
  and resets the request to the effective state. A per-call task sends the
  requested state on every reconnect. New `self_muted()`, `self_deafened()`
  and `VoiceStats::sent_packets`.
- Desktop: the client map is a private std mutex of `Arc<Client>`. Commands
  clone a client out with `AppState::client` and await without the lock. The
  guard is `!Send`, so holding it across an await in a command or spawned
  task does not compile, and clippy `await_holding_lock` rejects it in tests.
  This is the lint for "no command holds the client-map lock across a network
  await". Removing a client calls `state::close`, which aborts it even while
  a command still holds a handle.
- Mute/deafen from the dock, menus, global shortcuts and tray go through
  `voice::set_self_state`, which never takes `operations`. A mute that cannot
  reach the server counts as done; a failed unmute is reported. The UI flags
  mirror the effective gate.
- Global PTT reads connection state from the call's own `StateStore`, not
  through the client map. The arming state machine is `input::PttArming`
  with unit tests; `drive` was split into small functions.
- The in-app PTT button sends each change only after the previous invoke
  settled (`features/voice/api.ts`).

Deviations and files outside Paths:
- `ss-client-core/src/connection.rs` (T-081's file): added
  `Client::abort(&self)`, six lines, next to `close`. Without it a shared
  client could not be stopped while a queued `SendMessage` held a handle
  during reconnect, so it could rejoin voice after an explicit leave.
- Every desktop module that used the client map changed mechanically
  (`admin`, `attachments`, `audit`, `channel_admin`, `diagnostics`,
  `moderation_admin`, `notifications`, `onboarding`, `role_admin`, `search`,
  `server_settings`, `stream`, `viewer`, `webhooks`, `state`, and tests).
  `load_history` delegates to `commands::history` for testing.
- `set_self_mute`/`set_self_deaf` are no longer `async fn` but return
  `impl Future + Send + 'static`; `.await` callers are unchanged.
- Kept the existing arming rule that a press for a call the 50 ms tick has
  not seen yet needs a release first. T-095 may want to revisit it.
- After the gate closes the graph still sends one end-of-burst frame
  (`graph.rs`, T-082) holding audio captured before the mute call.

Tests: `ss-server/tests/client_core.rs`
`mute_closes_the_gate_before_the_server_and_reconciles_after_reconnect`
(gate closed before the future is awaited; mute while reconnecting stops
frames for a full second; server shows self-mute after reconnect; failed
unmute keeps the gate closed). Desktop
`ptt_and_mute_never_wait_for_slow_requests_or_connect` (global PTT press
while a history request stalls on an HTTPS port that never answers;
mute/deafen toggles while `operations` is held). `PttArming` unit tests.
Vitest `features/voice/api.test.ts`. `just check`, `cargo test` and clippy
for `socialspeak-desktop`, and tsc, eslint, prettier and vitest from
`apps/desktop` pass; the lockfile is unchanged.

Hands-on checks in the installed app:
- In voice with a global mute key, start connecting to a slow or unreachable
  second server and press mute: the dock and tray show muted at once and the
  other side stops hearing you.
- Turn the network off during a call, mute, turn it back on: after the
  reconnect other users see you muted.
- Hold global PTT while scrolling history or sending on a slow server: the
  first press opens the gate.
- Tap the in-app PTT button rapidly: it never stays transmitting.
- Disconnect a server while a message send is queued during reconnect: it
  does not come back or rejoin voice.
