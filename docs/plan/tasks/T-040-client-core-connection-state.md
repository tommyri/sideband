# T-040 — `ss-client-core`: connection manager and state store

Status: todo
Phase: 3
Size: M
Depends on: T-004, T-021, T-023, T-024, T-026
Paths: `crates/ss-client-core/src/connection.rs`, `crates/ss-client-core/src/state/`, `crates/ss-client-core/src/events.rs`

## Goal
A headless, UI-agnostic client library that maintains a live mirror of the
server state and exposes a command API plus an event stream. Both the Tauri
shell and `ss-cli` are thin wrappers over it.

## Scope
- `Client::connect(ServerAddr, Credentials, PinStore) -> Client` using
  `ss-net`; `Hello`/`Welcome`; applies `Snapshot` into `StateStore`.
- `StateStore` (`Arc<RwLock<…>>` or `arc-swap` snapshots): server info,
  channels tree (sorted), roles, members + presence, voice states, tracks,
  read states, per-channel message cache (ring of last 200 + loaded pages),
  DM list, own user + resolved permissions per channel.
- Event application: every `ServerEnvelope` event mutates the store and
  emits a `ClientEvent` (coarse: `ChannelsChanged`, `MessagesChanged{channel}`,
  `VoiceChanged`, `MembersChanged`, `Connection(state)`, `Notice`…).
- Commands mirroring the proto with typed results (`send_message(...) ->
  Result<MessageId>`), using `ControlStream::call`.
- History: `load_older(channel, before)` via HTTPS REST with the session
  token and pinned cert (`reqwest` with the same verifier).
- Reconnect: exponential backoff (1 s → 30 s, jitter), `resume_token`,
  re-join voice channel if one was active; `ConnectionState` enum emitted.
- Multi-server: `ClientSet` managing several `Client`s with ids.
- Persisted: pin store, session tokens per server, last joined server (via a
  `Storage` trait implemented by the host).

## Out of scope
Audio (T-041), UI.

## Acceptance criteria
- Integration tests against an in-process `ss-server`: connect, snapshot
  applied, send/receive message, channel created event reflected, forced
  server restart → reconnect and state re-synced.
- No `tokio::spawn` leaks on `drop(Client)` (use `JoinSet`/`CancellationToken`).

## Notes from implementation
