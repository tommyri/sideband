# T-024 — Connection registry, presence, event fan-out

Status: done
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

- `ConnectionRegistry` maps `conn_id` to the QUIC connection, client info from
  `Hello`, a bounded outbound queue, and the latest QUIC RTT sampled on
  `Ping`. `user_id` maps to the live connection ids. The voice-spike accept
  loop is unchanged.
- The first connection emits one `PresenceUpdated`. Another connection for
  the same user emits nothing. The last disconnect emits offline with an
  empty activity. `SetPresence` (client field 34) accepts online, idle, or
  do-not-disturb and rejects offline. The chosen status is remembered until
  the process restarts and comes back on the next connect. `Welcome`
  features include `presence`. Handler tests still call `snapshot::build`,
  which reports everyone offline; the accept loop uses `build_with`.
- Domain events go on an `EventBus`. The actor still receives them on the
  command reply. Every other session skips that copy and applies
  `snapshot::delivers`, so a channel event is not sent to someone who cannot
  view the channel. A delete is still not fanned out: the channel is already
  gone, which is the T-023 rule.
- The outbound queue holds 32 frames. Fan-out uses `try_send`. A queue that
  stays full for 10 seconds, or a control write that stalls that long, sends
  `ServerNotice` and closes that connection with QUIC code 2. Other
  connections are left up. Tests shorten the grace. Shutdown sends
  `ServerNotice` ("server shutting down") and, after a short grace so the
  frame can leave the socket, closes with QUIC code 1.
