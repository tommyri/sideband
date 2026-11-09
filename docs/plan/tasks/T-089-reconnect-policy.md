# T-089 — Reconnect policy: retryable errors, address fallback, backoff, stale cache

Status: todo
Phase: 4
Size: M
Depends on: T-060, T-081, T-086
Paths: `crates/ss-client-core/src/connection.rs`, `crates/ss-client-core/src/error.rs`, `crates/ss-client-core/src/state/`, `crates/ss-net/src/client.rs`, `crates/ss-net/src/pins.rs`, `docs/plan/02-protocol.md`

## Goal
A client that can recover does recover, and what it shows after recovering
is true.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
reproduced. Touches `connection.rs`; run after T-081 and T-086.

## Scope
- Any server `Error` to the resume `Hello` is terminal (`terminal()` matches
  `Error::Server { .. }`). `02-protocol.md` says a `RATE_LIMITED` Hello
  means reconnect after the wait, and the server allows 30 Hellos per
  address, so clients behind one NAT go to `Failed` for good after a server
  restart. `Error::Server` also drops `retry_after_ms`, `min_version` and
  `max_version`. Carry those fields; treat only `UNAUTHENTICATED`, `BANNED`,
  `FORBIDDEN` and `PROTOCOL_VERSION` as terminal; sleep `retry_after_ms`.
- Addresses are tried one after another and quinn has no handshake timeout
  below the 30 s idle timeout, while every caller wraps `connect` in
  10–15 s. A server with A and AAAA records reachable over one family only
  always times out. Add a per-address handshake timeout of about 3 s in
  `connect_resolved`, or race the addresses.
- Backoff: `(base + jitter).min(30_000)` removes the jitter once the base
  reaches 30 s; `attempt` resets on any Welcome, so a session that dies
  right after resume flaps every two seconds; `Runner.pending` is unbounded
  while reconnecting. Jitter below the cap, reset only after a stable
  period, cap the queue.
- Reconnect keeps the message cache and fetches only `?after=<newest id>`,
  so edits and deletes made during the outage never arrive, and the resync
  stops at 20×50 messages per channel leaving a silent gap. Reload the
  newest page for open channels on resume or add a delta endpoint, and mark
  a gap when the cap is hit. Record the choice in the T-060 notes.
- `compare_exchange(policy, policy)` rewrites and fsyncs the trust store on
  every successful connect. A read-only or full disk then fails every
  reconnect although trust is unchanged. Skip the write when nothing
  changed.
- The client never inspects the server's application close code. Use it to
  tell shutdown, kicked and rate-limited apart when the queued event did
  not arrive first.
- Unconfirmed, check with tests: a mention arriving between the Welcome
  snapshot and that channel's resync is neither counted nor notified; a
  `SendMessage` replayed after more than five minutes outlives the nonce
  window and duplicates.

## Out of scope
Media pump and heartbeat (T-081). Router tolerance (T-101).

## Acceptance criteria
- Tests: `RATE_LIMITED` Hello is retried after the stated wait; a banned
  Hello is terminal; a black-holed first address falls back within five
  seconds; a message deleted during an outage is gone after resume.
- Reconnecting with an unchanged policy performs no trust-store write.
- `02-protocol.md` states which Hello errors are retryable.

## Notes from implementation
