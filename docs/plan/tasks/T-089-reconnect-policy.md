# T-089 — Reconnect policy: retryable errors, address fallback, backoff, stale cache

Status: awaiting-acceptance
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

Implemented 2026-11-10. Each finding got a test that failed first, for the
stated reason. Nothing was refuted, but the first "unconfirmed" item is
real for a different reason than the review gave.

| Finding | Test | Before the fix |
|---|---|---|
| `RATE_LIMITED` resume is terminal | `pump_tests::a_rate_limited_resume_is_retried_after_the_stated_wait` (scripted server refuses one resume with 1000 ms) | `Failed`, never retried |
| Banned resume | `pump_tests::a_banned_resume_is_terminal` | passed before and after; it guards the narrower `terminal()` |
| `Error::Server` drops the wait and version range | `error::tests::server_errors_keep_the_wait_and_the_version_range` | fields did not exist (code reading) |
| Black-holed first address | `ss-net client::tests::a_black_holed_first_address_falls_back_within_five_seconds` (bound UDP socket that never reads, then the server) | still waiting after 5 s |
| Jitter lost at the cap | `connection::tests::backoff_keeps_its_jitter_at_the_cap` | 64 of 64 delays were exactly 30 s |
| Backoff resets on any Welcome | `pump_tests::a_session_that_dies_right_after_resume_backs_off_further` | 2.28 s after a session that lasted no time |
| `pending` unbounded | `connection::tests::the_reconnect_queue_is_bounded` | 101 of 101 queued |
| Stale cache after resume | `ss-server tests/client_core.rs::edits_and_deletes_made_during_an_outage_show_after_resume` (soft delete and edit in the database while the client is away) | `Connected` with the deleted message and the old text |
| Trust store written on every connect | `ss-net client::tests::a_connect_with_unchanged_trust_does_not_write_the_store`, `pins::tests::an_unchanged_policy_is_not_written_again` (target path made a directory) | 2 writes for 2 connects; `IsADirectory` |
| Close code ignored | `pump_tests::close_codes_tell_kicks_and_sign_outs_from_restarts` (codes 1–4, 6 with no event first) | a kick reconnected |
| Unconfirmed: mention not counted or notified | `connection::tests::a_mention_a_resync_page_brought_first_is_counted_and_announced_once` | count `None`, not notified |
| Unconfirmed: replay past the nonce window | `pump_tests::a_send_older_than_the_nonce_window_is_not_replayed` (Ack withheld, wall clock +6 min) | replayed with the same nonce |

On the two unconfirmed items:
- Mention: a mention that arrives live after the Welcome is counted and
  notified, as long as its `MessageCreated` is applied first. It is lost
  when the resync page that contains it is merged first: the page comes from
  the database over HTTPS, the event over QUIC, and the loop's `select!`
  takes either. The cached page copy then made the event look like an echo,
  so it was neither counted (the snapshot was built before the message) nor
  notified. The reproduction is at the state level; the end-to-end race was
  not reproduced.
- Replay: real. The server deduplicates nonces for 5 minutes
  (`handlers/messages.rs`, `NONCE_WINDOW_MS`) and the client replayed with no
  age limit. A Mac asleep with an unacknowledged send is the likely case,
  since `Instant` stops during sleep.

**What changed**

- Protocol: no `.proto` change and no new tags. `02-protocol.md` gets §1.2
  "Reconnecting": which resume errors are final (`UNAUTHENTICATED`,
  `BANNED`, `FORBIDDEN`, `PROTOCOL_VERSION`) and which are retried
  (`RATE_LIMITED` after `retry_after_ms`, everything else after the
  backoff). It also covers the backoff, the per-address timeout, the queue
  cap, the replay window, the resync, and the client's handling of close
  codes 3, 4 and 6. `01-architecture.md` has one updated sentence on the
  resync.
- Server: no source change. The test `World::restart_after` now closes with
  code 1, as `serve` does on shutdown.
- `ss-net`: `connect_resolved` gives each address but the last 3 s for its
  handshake (`quinn::ConnectionError::TimedOut`); the last one keeps the
  caller's deadline, so a slow single-address link is not cut short. A
  stored policy is re-read and compared, not written. Only a first-use
  public certificate writes. `JsonPinStore::compare_exchange` also skips the
  write when the new policy is already stored.
- `Error::Server` carries `retry_after`, `min_version` and `max_version`.
- `connection.rs`:
  - `terminal()` treats only the four final codes as final.
  - The reconnect loop moved to `Runner::reconnect`. A `RATE_LIMITED`
    refusal waits `retry_after` plus up to 25 %, so clients refused together
    spread out. The wait is capped at 10 minutes against a broken server.
  - Backoff is 2, 4, 8, 16, then 24 s, each plus up to 25 %. The level
    survives across sessions and resets only after a session of 30 s or a
    close with code 1 (shutdown). Code 5 (media rate limit) never resets it.
    `Reconnecting.attempt` still counts from 1 in each outage, so the
    desktop's "connection lost" cue is unchanged.
  - When the connection ends, events already read are applied first. Then
    close code 3, 4 or 6 emits `Removed` and ends `Disconnected` without
    reconnecting.
  - `pending` holds at most 100 sends. Past that, sends fail with `Capacity`.
  - Each queued send records when it first went out (`Pending.sent`, both
    clocks). One sent more than 4 minutes ago gets `Timeout` instead of a
    replay. A send that never went out is always sent.
- Cache resync, the choice (also recorded in the T-060 notes): no delta
  endpoint. Soft deletes would allow one, but it would need a new route and
  an edit cursor. On resume, every channel with cached messages is re-read
  from the newest page backwards (`before=`) until a page reaches the oldest
  cached message, at most 4 pages (200 messages). Each page is
  authoritative for its id range (`state::Span`). A cached message in the
  range that the page lacks is removed, unless a live event brought it
  during the fetch (`Resync.live` now records creates too). Page entries
  replace cached copies, so edits show. When the page budget runs out
  before the old cache, everything older is dropped. The cache never has a
  gap, so nothing needs marking, and older history loads on scroll as
  usual. A message first seen in a page is marked unannounced, so its live
  `MessageCreated` still counts and notifies once.

**Files outside the Paths**

- `apps/desktop/src-tauri/src/commands.rs`: `..` in the `Error::Server`
  pattern, forced by the new fields.
- `crates/ss-client-core/src/connection/pump_tests.rs`: `connection.rs`'s
  test module. The scripted server can now refuse `Hello`s, report when
  each `Hello` arrives, and record the commands it receives.
- `crates/ss-server/tests/client_core.rs`: the outage test, and the shutdown
  code in `World::restart_after`.
- `docs/plan/01-architecture.md`, `docs/plan/tasks/T-060-reconnect-resilience.md`,
  `docs/plan/tasks/INDEX.md`.

No files in `voice/` or the desktop frontend changed.

**Tests**

`just check` passes; the known flaky
`webhook_management_lists_metadata_and_revokes_the_created_url` failed in
one run and passed in the rerun. `cargo clippy -p socialspeak-desktop
--all-targets -- -D warnings` passes. The ignored two-minute chaos test
passes. An earlier draft failed it with 6–7 cycles: the close code was read
after the client's own `close`, which replaces the peer's reason, so the
backoff never reset.

**Open questions and follow-ups**

- Possible double count, not verified: the server subscribes a session to
  the bus before it builds the snapshot. A mention stored in between is
  counted in the snapshot's `mention_count` and again by its live event.
- The resync costs up to 4 HTTPS requests per cached channel, sequentially.
  Many clients behind one NAT, each with many cached channels, can pass the
  HTTP quota (200, then 50/s per address). A `429` defers that channel with
  a notice and leaves its cache as it was. Honouring `Retry-After` in the
  resync would close this.
- When the 4-page budget runs out, the open channel loses its older loaded
  messages, which may move the scroll position.
- For hands-on testing: a network with broken IPv6 and a dual-stack host
  should connect in about 3 s; restart a server with more than 30 clients
  behind one address, and all of them should come back.
