# T-081 — Keep voice datagrams flowing while control commands are pending

Status: todo
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
