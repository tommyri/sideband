# T-081 — Keep voice datagrams flowing while control commands are pending

Status: awaiting-acceptance
Phase: 4
Size: M
Depends on: T-041, T-060
Paths: `crates/ss-client-core/src/connection.rs`, `crates/ss-client-core/src/history.rs`, `crates/ss-net/src/lib.rs`, `crates/ss-server/tests/voice_session.rs`

## Goal
Audio never waits for the control stream. Today every command pauses voice
in both directions for its whole round trip.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
reproduced. Start by writing the failing test in the acceptance criteria.

## Scope
- `Runner::connected` awaits `execute()` inside its `select!` command arm
  (`connection.rs` around line 588). `execute` polls only the Ack and control
  events, so `outgoing.recv()` and `read_datagram()` are not polled until the
  Ack arrives (deadline 10 s). Typing, MarkRead, SendMessage and
  SetVoiceState all do this during a call. Inbound packets are also stamped
  `at: Instant::now()` late, which inflates the receiver's jitter estimate.
  Move datagram send and receive to their own task tied to the live
  connection, or make the pending command a select branch.
- After a reconnect, `resync_messages` runs sequential HTTPS history fetches
  before the loop restarts (new TLS connection per page, 10 s timeout, one
  per cached channel). Voice is rejoined on the server but silent, and
  nobody drains `incoming`; more than 256 queued events closes the new
  connection with `Capacity`. Enter `Connected` first and run the resync
  concurrently with event draining and the media pump.
- Heartbeat: `last_tick` only advances when the heartbeat arm runs, so a
  command acknowledged after 3–10 s makes `clock_jumped` reconnect a healthy
  connection. Take the heartbeat out of the path that awaits commands.
- Sleep detection: `Instant` does not advance during sleep on macOS, so the
  T-060 clock-jump check cannot see a real sleep there. Compare `SystemTime`
  against `Instant` deltas, or take an OS wake notification from the shell.
- Datagram send buffer: `transport_config` sets 4 MiB for send. Reported, not
  confirmed: quinn drops the oldest datagram only when the buffer is full, so
  a congested link queues minutes of stale audio. Size the send buffer for
  roughly 100–200 ms of audio and keep the receive buffer large; confirm the
  drop behaviour against the pinned quinn before changing it.

## Out of scope
Reconnect policy and error handling (T-089). Jitter-buffer changes (T-083).

## Acceptance criteria
- Integration test: with an impaired control stream (Ack delayed 500 ms),
  a client in voice sends a chat message; datagrams continue in both
  directions with no gap above one frame interval.
- Integration test: reconnect with ten cached channels and a slow history
  endpoint; audio resumes before the resync finishes and the connection is
  not closed for capacity.
- A slow Ack no longer triggers a reconnect; a simulated wall-clock jump
  does.
- `docs/plan/01-architecture.md` client runtime model describes the pump.

## Notes from implementation

All four findings were confirmed by failing tests before the fix. On the old
runner a 500 ms Ack held datagrams back by about 482 ms in both directions
(485 ms against the real server). A 4 s Ack made the heartbeat reconnect. A
reconnect with ten cached channels and a stalled history endpoint stayed
`Reconnecting`, and the control router then closed the connection: code 2,
"control reader stopped", which is the `Capacity` overflow path in
`ss-net::control`.

What changed:
- `MediaLink::pump` (`connection.rs`) is a task per live connection. It
  owns datagram send and receive and the 500 ms stats tick, and stamps
  `IncomingMedia::at` when the datagram is read. `Live` owns the task and
  aborts it on drop. It starts at connect, and on resume right after
  `restore_voice`. It does not start earlier, so no datagrams go out with a
  track the server has not yet given back.
- The connection loop (`Runner::serve`) has no datagram branches. The one
  in-flight command is an `InFlight` select branch. A new command is taken
  only when none is in flight, so commands keep their order. `begin` and
  `finish` hold the old per-command rules (stale generation, Typing throttle,
  replay and stop decisions). If the connection ends while a replayable send
  is in flight, the send is queued for replay with its nonce, as before.
  `execute` is now used only during reconnect setup (voice rejoin and
  pending replay). Every awaited setup step, including the session `persist`,
  now runs under `draining`, so events cannot pile up in the router before
  the loop starts.
- Heartbeat: it is no longer behind a command, so a slow Ack cannot fire it.
  `clock_jumped` compares `Instant` deltas with `SystemTime` deltas, since
  macOS `Instant` stops during sleep. A drift over 3 s, either way,
  reconnects, and so does a stall of `Instant` itself. `Clock` carries a
  test-only wall-clock skew.
- Resync: the reconnect reports `Connected` (and `Resynced`) after the
  snapshot, voice rejoin and pending replay. `Resync` then fetches `?after=`
  pages on its own task, with one pinned `history::History` client for the
  whole run (`fetch_page` is now a wrapper around it). The loop merges each
  page between live events. `Resync` keeps the live `MessageUpdated` and
  `MessageDeleted` events seen since it started. A page entry that a live
  edit or delete has already replaced is swapped for the live copy or
  dropped, so a page cannot bring back an older version. Dropping `Resync`
  (connection ended) aborts the fetch. `StateStore::resync` now checks the
  snapshot generation instead of requiring `Reconnecting`.
- Send buffer (`ss-net/src/lib.rs`): with the pinned quinn 0.11.12 /
  quinn-proto 0.11.19, `send_datagram` calls `send(data, drop = true)`, and
  `DatagramState::make_space_for` drops the oldest queued datagram only once
  the buffer is full. The old 4 MiB was about six minutes of 80 kb/s voice.
  The client buffer is now 3 KiB. That holds 240 ms at voice 80 kb/s, 160 ms
  of music at 128 kb/s, 100 ms at 192 kb/s and 250 ms of low-latency audio.
  A unit test checks those figures against `VoiceProfile`. The server buffer
  is 16 KiB, because it carries every speaker to each member (about 200 ms
  for six default-profile speakers). Receive buffers stay at 4 MiB.

Tests:
- `ss-client-core/src/connection/pump_tests.rs` uses real QUIC against a
  scripted ss-net server, which fixes the Ack delay exactly and can stall
  history and flood events. It covers four cases. First, a 500 ms Ack while
  a datagram goes each way every 20 ms: no loss, p95 one-way delay under one
  frame and the maximum under three. Second, a 4 s Ack does not reconnect.
  Third, a 60 s wall-clock skew reconnects with the resume token. Fourth,
  after a reconnect with ten cached channels, a history port that accepts
  and never answers, and 400 events trickling in over about a second, the
  client is `Connected`, audio flows both ways within those bounds while the
  resync is still pending, and the connection stays open. Receivers stop at
  a deadline and allow two lost datagrams, since loopback UDP can drop one
  under load. A unit test checks that resync
  pages give way to live edits and deletes.
- `voice_session.rs`, real server: a client in voice with the test tone
  sends a chat message while the test holds the SQLite write connection for
  500 ms. A raw peer in the channel sees the arrival-minus-media-time spread
  stay under 100 ms. On the old runner it was 485 ms.
- `just check` passes (fmt, clippy `-D warnings`, workspace tests) on
  aarch64-apple-darwin.

Deviations:
- The bound is "p95 under one frame, maximum under three frames", not a
  strict one-frame maximum. One early run hit a single 30 ms scheduler
  hiccup in both directions at once, with the crate's tests running in
  parallel in a debug build. The old behaviour holds about 25 frames back by
  up to 500 ms, so the test still tells the two apart.
- The precise timing tests live in `ss-client-core`, not `voice_session.rs`.
  They need the client-side arrival stamp and an exact Ack delay, which the
  real server cannot provide without a test hook. `voice_session.rs` has the
  end-to-end check for the sending direction.
- Files outside the listed paths: `state/mod.rs` (the `resync` guard and its
  unit test) and `client_core.rs`. The forced-restart test now waits for the
  missed message instead of asserting it at `Resynced`, because history now
  arrives after `Connected`. `over_bitrate_sender_is_disconnected…` in
  `voice_session.rs` now paces its 24-datagram flood by 1 ms. Sent in one
  synchronous burst, the new 3 KiB buffer would keep only its newest entries,
  which is the intended drop-oldest behaviour.
- Small behaviour change: if a server `Removed` event arrives while a
  command is in flight, the client now ends `Disconnected`, the same as when
  no command is in flight. Before, it ended `Failed`. The command's caller
  gets `Disconnected`.

Follow-up (not in scope):
- The control router's reader `try_send`s into a 256-slot channel and closes
  on overflow by design. A burst of more than 256 events arriving within a
  millisecond, right after the Welcome, can beat even an actively draining
  loop, and the connection closes with `Capacity`. That happened with an
  unpaced 400-event flood in about one run in four. Each event is applied
  under the store lock as it is read, so the client cannot catch up with
  such a burst. A larger or adaptive buffer, or batching in the reader,
  belongs with T-101 (router tolerance) or its own task.

Left for hands-on testing (no second machine or VPS here):
- Two desktop clients in a call through a remote server. Type, send
  messages, mark read and toggle mute while the other side listens. There
  should be no dropouts and the jitter delay in the stats panel should not
  grow.
- Close the lid on a Mac in a call for at least 10 s and reopen it. The
  client should reconnect within about 1 s of the next heartbeat, not after
  the 30 s idle timeout.
- Restart the server while in a call with several text channels that have
  history. Voice should come back as soon as the client shows `Connected`,
  and the missed messages should appear shortly after.
- On a congested uplink (for example a link conditioner at 100 kb/s), audio
  latency should stay bounded (old audio dropped) instead of growing.
