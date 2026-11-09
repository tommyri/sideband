# T-101 — Control router tolerates what the protocol allows; fuzz the real parsers

Status: todo
Phase: 4
Size: M
Depends on: T-004, T-061
Paths: `crates/ss-net/src/control.rs`, `crates/ss-net/src/media/stream.rs`, `crates/ss-client-core/src/state/mod.rs`, `fuzz/fuzz_targets/`, `docs/security.md`, `docs/plan/02-protocol.md`

## Goal
Old clients survive new servers, the server's own notices do not kill the
connection, and the fuzz targets exercise the code that runs in production.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
reproduced.

## Scope
- The client router treats an `Ack` or `Error` whose payload id is zero as
  "response ids disagree" and closes. The server sends exactly that for
  unsolicited notices (`rate_limited(0, "media bitrate exceeded", ..)` in
  `ss-server/src/connection.rs`), and `envelope.proto` documents zero ids.
  `ClientEvent::ServerError` is therefore unreachable and the wait hint is
  lost. Route id-zero errors to the event channel.
- Forward compatibility: a `ServerEnvelope` with an unknown oneof variant
  decodes to `msg: None` and is fatal; an unknown bit in
  `effective_permissions` fails `Permissions::from_bits`, which fails
  Welcome. `02-protocol.md` §7 promises fields are only ever added and the
  server has no client capability signal. Ignore unknown variants (count
  them), keep unknown permission bits raw or truncate them.
- The `control_frames` fuzz target drives `ss_proto::read_frame`, which
  only tests call. Both peers use the incremental parser
  `ControlReceiver::read_frame`. The video group parser (`read_header`,
  `GroupReader::read_frame`) has no target. Extract both as sans-IO state
  machines over byte chunks and fuzz those, with arbitrary chunk
  boundaries.
- Correct `docs/security.md` §10, which says chunked control reads are
  covered.

## Out of scope
The one-hour fuzz runs (T-067). Server-side tolerance of unknown client
messages unless the same defect exists there; check and note it.

## Acceptance criteria
- Test: an unsolicited id-zero `Error` reaches the application as
  `ServerError` with its retry hint and the connection stays up.
- Test: an envelope with an unknown variant and a Welcome with an unknown
  permission bit are both accepted by today's client.
- Two new fuzz targets build and run for five minutes clean; `fuzz.yml`
  lists them.

## Notes from implementation
