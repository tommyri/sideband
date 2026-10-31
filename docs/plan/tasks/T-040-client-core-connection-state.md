# T-040 — `ss-client-core`: connection manager and state store

Status: done
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

Implemented on `task/T-040-client-core-connection-state`; awaiting review and
merge. Status stays in-progress until merged. Nothing pushed.

- `Client::connect` returns after authenticated Welcome and snapshot application.
  `connect_with_storage` adds host-owned session/last-server persistence; the
  existing `PinStore` remains the certificate persistence boundary. Credentials
  have no Debug implementation, and passwords are dropped after authentication.
- Read-only `StateStore` handles expose a consistent `RwLock` view or an owned
  copy. The reducer handles every server event; tree order is deterministic,
  voice is keyed by connection id, and message echoes deduplicate by id. Each
  channel retains 200 live messages plus requested historical pages. Visibility
  revocation and reconnect purge cached content.
- All application protocol commands have typed wrappers over the bounded
  control router. Commands rejected by the server leave the session usable.
  Typing is limited to one send per channel per five seconds. Commands are never
  replayed; queued work is tagged with its originating session generation.
- Resume retries start at one second with up to 25% jitter, doubling to a
  30-second cap. A fresh snapshot replaces stale state before returning to
  Connected; requested voice membership and mute/deaf flags are rejoined.
  Authentication, certificate, protocol, kick, and ban failures stop retries.
  Each attempt and control request has a ten-second deadline. Drop aborts the
  owner, which closes QUIC and cancels the control workers; `close().await`
  waits for the owner. `ClientSet` owns independent clients by server identity.
- `load_older` requests 50 messages over HTTPS using the Welcome bearer and the
  exact QUIC leaf pin, reusing ss-net's signature-verifying TLS verifier. HTTP
  redirects and environment proxies are disabled, and bodies are capped at
  4 MiB. The actual server JSON shape is decoded, including base64 ids and
  string 64-bit integers. If state changes during a request, `StateChanged`
  asks the caller to retry rather than resurrect deleted/edited content.
- Protocol addition: `Channel.effective_permissions`, optional uint64 tag 12.
  Clients cannot resolve channel permissions from the old snapshot because
  overrides were not transmitted. The server now personalizes channel views
  and diffs them after role, grant, override, and channel changes. Newly visible
  channels use Created; revoked visibility uses Deleted with the old visible
  record. The wire notes and D10 explain this and shared HTTPS trust.
- Audio orchestration remains T-041. The T-017 CLI voice-spike harness is kept
  intact; this task supplies the authenticated connection/state API for hosts.
  Production HTTPS is the existing reverse-proxy boundary. Tests wrap the real
  Axum history router with TLS using the same identity as QUIC.
- Regression coverage includes snapshot/cache/event application, tree ordering,
  real two-client messages and edits, channel creation/deletion, permission
  denial without disconnect, visibility revoke/restore, persisted login, forced
  restart and voice rejoin, client drop, ban stopping reconnect, first-use trust,
  authenticated HTTPS pagination, and rejection of a different HTTPS leaf.

Validation on macOS: `just check` passed (262 tests, 1 existing manual
load test ignored); `just check-deps` and `git diff --check` passed. The
server regression suites also passed independently. No microphone, speaker,
external server, or production account was used.
