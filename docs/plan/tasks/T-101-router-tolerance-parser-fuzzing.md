# T-101 — Control router tolerates what the protocol allows; fuzz the real parsers

Status: done
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

All three findings were confirmed by failing tests before the fix:

- Id-zero `Error`: the router stopped with `Protocol("response ids disagree")`
  and closed the connection with code 2. Its local close also hid the
  server's code 5, so the T-089 rule "a media rate-limit close never resets
  the backoff" could not apply.
- Unknown `msg` variant: the router stopped with `Protocol("missing server
  message")`. The client reconnected, and any command in flight failed with
  a terminal error (`Failed`).
- Unknown permission bit: `StateStore::welcome` failed with `Protocol("invalid
  permissions")`, which is terminal. A `ChannelCreated` or `ChannelUpdated`
  event failed the same way. Had the mask got through, `State::permissions`
  (`from_bits`) would have returned `None`, and the desktop treats that as
  no access.

What changed:

- `ss-net::control`: `route()` decides each envelope. Id-zero `Error` is an
  event. Id-zero with no message (an unknown variant) is skipped and counted
  in `ClientControl::unknown_messages()`. A nonzero id with no message still
  goes to its waiting request, so only that request fails. An `Ack` at id
  zero stays a violation ("unsolicited Ack"): `envelope.proto` documents only
  `Error` there. Mismatched Ack/Error ids still close.
- Permission bits are **kept raw**. The `Channel` and `Role` copies in
  `State` hold the server's mask unchanged. `State::permissions` returns
  `from_bits_truncate`, so an unknown grant enables nothing in this build
  (fail-closed). Truncating at the mirror would have been the wrong choice:
  role editing round-trips masks as decimal strings, and the frontend's
  `BigInt` toggles keep bits it does not know. Truncation would silently
  strip a grant the admin never saw. Gap left open: the desktop's
  `role_admin::mask` still rejects unknown bits on save ("Invalid
  permissions"), so a role from a newer server can be viewed but not saved
  (outside this task's paths).
- The server was checked for the same defect and does not have it. A
  `ClientEnvelope` with an unknown variant gets `INVALID` "unsupported
  command" and the connection stays up. There is now a test for this in
  `hardening.rs`. `connection.rs` is unchanged (T-094 owns it).
- Sans-IO parsers: `ss_net::control::FrameDecoder<M>` (`wanted`, `feed`)
  and `ss_net::media::stream::GroupParser` (`wanted`, `feed`, `finish`).
  Both take chunks of any size and never consume past the current item.
  Their errors are sticky. `ControlReceiver::read_frame`, `read_header`,
  and `GroupReader` drive them with `read_chunk(wanted)`, so they are still
  cancellation-safe. While reading the prefix the group parser wants one
  byte at a time, so `read_header` still leaves frame bytes in the stream
  for the relay. Behaviour change: frame payload memory now grows with the
  bytes that actually arrive (64 KiB first, doubling, capped at the declared
  length). Before, a 9-byte prefix declaring 4 MiB zero-filled 4 MiB at
  once.
- Fuzzing: new `control_stream` (the `FrameDecoder` in both directions,
  compared with a whole-buffer reference parse) and `video_group`
  (`GroupParser`; chunked output equals a single feed, and items re-encode
  to exactly the bytes they consumed). In both, the first byte gives the
  number of cut bytes, and zero-length chunks are fed too. `control_frames`
  is kept but retargeted to one-frame protobuf decode and round trip. Its
  chunked `ss_proto::read_frame` pass is dropped, since `control_stream`
  covers the production parser. `fuzz.yml` lists all seven targets.
  `docs/security.md` §10 was corrected (it also said "nightly"; the workflow
  is manual dispatch).
- Fuzz runs: `cargo-fuzz` 0.13.2, nightly, ASan, `-timeout=10
  -rss_limit_mb=2048 -max_len=8192`. Both targets ran at the same time
  alongside a workspace clippy, so exec/s is low. `control_stream`: 301 s,
  1,652,422 runs, clean. `video_group`: 301 s, 1,658,080 runs, clean. The
  retargeted `control_frames` ran a 61 s smoke with 6,338,007 runs, clean.
  The first 20 s smoke of both new targets "timed out" on a harness bug: an
  all-zero cut list never advanced. The harness was fixed and those inputs
  rerun clean. No parser defect was found.
- Tests: `pump_tests::unsolicited_errors_and_unknown_events_keep_the_connection`
  (real QUIC, client-core end to end, retry hint 1500 ms),
  `state::tests::permission_bits_from_a_newer_server_are_kept_raw_and_read_as_known`,
  `loopback::router_delivers_unsolicited_errors_and_skips_unknown_events`,
  and unit tests for both parsers and `route()`.

Follow-ups, not done:

- `fuzz/Cargo.lock` is stale on `main`. Every fuzz build rewrites it
  (about 700 lines). It is not committed here.
- The desktop `ServerError` payload drops `retry_after_ms`, so the UI cannot
  show the wait. `ClientEvent::ServerError` carries it.
- `role_admin::mask` in the desktop should accept unknown bits and let the
  server validate them.
- The T-081 follow-up (a 256-event burst can overflow the router) is not
  addressed here.
