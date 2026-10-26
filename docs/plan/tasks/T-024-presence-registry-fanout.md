# T-024 — Connection registry, presence, event fan-out

Status: todo
Phase: 2
Size: M
Depends on: T-021
Paths: `crates/ss-server/src/registry.rs`, `crates/ss-server/src/events.rs`, `crates/ss-server/src/handlers/presence.rs`

## Goal
The server knows who is connected, broadcasts presence, and delivers domain
events to the right connections efficiently.

## Scope
- `ConnectionRegistry`: `conn_id → ConnectionHandle { user_id, control_tx,
  quinn::Connection, client_info }`, `user_id → Vec<conn_id>`.
- Presence: online/offline derived from connections; `PresenceUpdated` on
  first connect / last disconnect; optional `status` (online/idle/dnd) from
  client via `UpdateProfile`-style message (add `SetPresence` to proto).
- `EventBus`: `broadcast::Sender<Arc<ServerEvent>>`; each connection task
  subscribes and applies a filter (`visibility(user, event) -> bool`) before
  encoding. Encoding happens once per recipient (unavoidable with
  per-recipient filtering) but payload `Bytes` are shared.
- Backpressure: per-connection outbound mpsc of bounded size; if a client
  can't keep up for 10 s, disconnect it with `ServerNotice`.
- `Ping/Pong` with server time; per-connection RTT sampled into registry for
  diagnostics.
- Graceful shutdown: `ServerNotice` then close all connections with a
  distinct QUIC error code so clients reconnect with backoff.

## Out of scope
Voice (T-025), text (T-026).

## Acceptance criteria
- Tests: two connections for the same user → one `PresenceUpdated(online)`;
  closing one → none; closing both → offline.
- Event filtering test: event in a channel invisible to B is not delivered
  to B.
- Slow-consumer test: a client that never reads is disconnected; others
  unaffected.

## Notes from implementation
