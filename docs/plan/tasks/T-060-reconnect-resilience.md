# T-060 — Network resilience: session resume, migration, restart handling

Status: done
Phase: 4
Size: M
Depends on: T-040, T-041, T-024, T-025
Paths: `crates/ss-net/`, `crates/ss-client-core/src/connection.rs`, `crates/ss-server/src/connection.rs`

## Goal
Wi-Fi hiccups, VPN toggles, sleep/wake and server restarts don't require
users to do anything.

## Scope
- Server-issued `resume_token` (short-lived, bound to session) in `Welcome`;
  on `Hello{resume_token}` within 60 s of the old connection's loss, the
  server re-attaches voice membership and track ids so peers see no
  leave/join (emit `VoiceStateUpdated` only on change).
- QUIC connection migration (`quinn` supports path validation): verify that
  a client IP change keeps the connection; if it does not within 3 s, trigger
  reconnect.
- Sleep/wake detection on clients (power events per OS or clock-jump
  heuristic) → immediate reconnect attempt instead of waiting for idle
  timeout.
- Server restart: clients receive `ServerNotice` + close code; reconnect
  backoff starts at 2 s and all clients rejoin within 30 s without a
  thundering-herd (jitter).
- State re-sync on resume: client sends last known `message_id` per visible
  channel; server returns deltas (add `ResyncRequest/Response` to proto) or
  falls back to full snapshot if too old.
- Message send during disconnect: queued locally with `nonce`, flushed on
  reconnect, deduped by server (nonce stored for 5 min per user).

## Out of scope
Multi-server failover.

## Acceptance criteria
- Tests: simulated address change keeps the session; killing the server
  for 5 s and restarting results in all test clients reconnected with voice
  restored and no duplicate messages.
- Chaos test in CI (toxiproxy or in-process impairment) runs for 2 min with
  random 1–10 s outages: no client stuck, no leaked tracks on the server.

## Notes from implementation

- A reconnectable close creates a process-local 60-second voice lease keyed by
  the authenticated resume token. Resuming transfers the new QUIC connection
  onto the original connection id and audio track id without broadcasting a
  leave/join. Normal closes, moderation closes, and expired leases still clean
  up immediately or at lease expiry.
- `ss-net::Connection::rebind` exposes Quinn's validated path migration. The
  loopback test replaces the client UDP socket and proves the existing control
  stream remains usable.
- The client uses a one-second monotonic heartbeat; a clock jump over three
  seconds enters reconnect instead of waiting for QUIC idle timeout. Backoff is
  jittered from 2 seconds and capped at 30 seconds.
- Re-sync uses the existing pinned HTTPS history API rather than adding another
  control-protocol exchange. Visible message caches survive snapshot install,
  and `?after=<message_id>` pages fill every known channel cursor before queued
  sends are flushed.
- `SendMessage` is the only replayable command. The client assigns a nonce,
  queues it while reconnecting, and the server stores the nonce and message in
  one SQLite transaction. Nonces expire from deduplication after five minutes.
- The restart integration test covers voice restoration, missed-message deltas,
  and exactly-once queued sends. CI explicitly runs the ignored two-minute
  in-process chaos test with random 1–10 second outages and asserts one restored
  voice seat plus one stored row per queued message.

### Control-stream close-reason regression — 2026-11-04

Full-suite validation for T-056A exposed a disconnect race: control I/O cleanup
called Quinn `close()` after a remote close, replacing its reconnect code with
`LocalClosed`. Whether control I/O or the datagram task finished first therefore
determined whether the server retained the voice seat. Control teardown now closes
only an open connection, preserving terminal peer/transport reasons. A deterministic
loopback regression covers reads and writes after a non-zero peer close; the
original voice-resume integration test also passes without weakening its timing.
