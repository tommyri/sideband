# T-060 — Network resilience: session resume, migration, restart handling

Status: todo
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
