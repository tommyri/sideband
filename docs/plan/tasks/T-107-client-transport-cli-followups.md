# T-107 — Client transport and CLI follow-ups from the review waves

Status: done
Phase: 4
Size: M
Depends on: T-089, T-101
Paths: `crates/ss-net/src/control.rs`, `crates/ss-client-core/src/connection.rs`, `crates/ss-client-core/src/history.rs`, `crates/ss-cli/src/`

## Goal
The defects that wave 1 to 4 tasks found outside their own scope, and left
untracked, are fixed or explicitly dropped.

Source: T-099, from the "Follow-ups" of T-081, T-082, T-086, T-089 and
T-101, each checked against the code at `0d0cb16`.

## Scope
- `ss_net::control` gives events an `mpsc` channel of 256
  (`ClientControl::start`) and `read_responses` closes the connection with
  `ControlError::Capacity` when it is full (`try_send`). A burst of more
  than 256 events while the connection loop is busy (a large category
  delete, a role reorder) drops the connection instead of applying
  backpressure. Make the router wait for room, or size and document the
  bound (T-081, T-101).
- `ss-cli` `call.rs` `note_track` finds its own track by `user_id`, so a
  second session of the same account mistakes the other's track for its own.
  Match `conn_id` as the client core does (T-086).
- `ss-cli` `graph.rs` still sends captured audio in the closing quantum after
  the gate closes, which T-082 fixed in `ss-client-core` (T-082).
- History resync after a reconnect ignores `Retry-After` on `429`, and
  mention counts may be counted twice between the bus subscription and the
  snapshot; the four-page budget may move the scroll position. Write the
  failing test first for each, or record why it cannot happen (T-089).

## Out of scope
Desktop shell changes (T-108). New reconnect policy.

## Acceptance criteria
- A test sends more than 256 events while a command is pending; the
  connection stays up or the documented bound is asserted.
- A test runs two CLI sessions of one account in one channel; each finds its
  own track.
- Each resync item has a test or a recorded reason it was dropped.

## Notes from implementation

Implemented 2026-11-11. Each item got a test that failed first, for the
stated reason, except the scroll position, which has no core behaviour to
test.

| Item | Test | Before the fix |
|---|---|---|
| Event burst past 256 | `ss-net` `loopback::an_event_burst_past_the_queue_waits_for_the_application` (1,000 events and then the Ack of a pending `call`, nothing read for 300 ms) | closed, code 2 "control reader stopped" |
| Same, end to end | `pump_tests::an_event_burst_while_a_command_is_pending_keeps_the_connection` (3,000 events in one write, Ack 300 ms later) | `Reconnecting` in 3 runs of 3 |
| CLI own track | `ss-cli` `it::two_sessions_of_one_account_in_one_channel_each_find_their_own_track` | the second session took track 1, the first's |
| CLI closing quantum | `graph::tests::closing_quantum_is_zeroed_even_when_the_microphone_still_hears_speech`, `a_one_quantum_burst_on_a_frame_boundary_is_sent` | captured audio in the `TALK_END` frame; a one-quantum burst sent nothing |
| Resync `Retry-After` | `client_core::a_rate_limited_resync_waits_as_told_and_then_catches_up`, `history::tests::a_429_carries_the_stated_wait` | resync deferred, the edit made while away never showed |
| Mention double count | `client_core::mentions_stored_while_a_session_signs_in_are_counted_once` (ignored, see below) | 21 to 24 counted for 20 stored, every run |

**Event queue: backpressure, not a bigger bound.** `read_responses` now
awaits room in the 256-event channel (`ss_net::control::EVENT_QUEUE`)
instead of `try_send`. While it waits it reads nothing, so QUIC flow
control holds the server back; the server already closes a consumer that
stays wedged for `SLOW_CONSUMER_AFTER` (10 s, code 2), which is the bound
that holds end to end. The wait is a `select!` with `connection.closed()`,
so the reader ends with the connection. A dropped event receiver now stops
the reader with `Closed` (`a_dropped_event_receiver_closes_the_connection`).

Why it cannot deadlock: replies queue behind the events in front of them,
as on the wire, so an application that awaits a reply without reading
events would wait until the server closes it. `ss-client-core` never does.
`Runner::serve` reads `incoming` in the same `select!` as the in-flight
reply, its event handling is synchronous, and every reconnect-setup reply
(`execute`, `persist`) runs under `draining`. `establish` awaits only the
`Hello` reply, and the server sends nothing before the `Welcome`. The router
is cancellation-safe as before: `recv` keeps partial frames, and the only
cancellation of the reader is the abort on drop. `ss-cli`'s `join_client`
awaited `JoinVoice` without reading, and the spike sends two events per
member before that Ack, so it now reads them (into `note_track`) while it
waits. `load::Session::join_voice` and `leave_voice` still await without
reading. Their replies come within one round trip, and a stall is the same
`Exit::Lost` as the old close. `ControlError::Capacity` stays for the
request table.

**CLI own track.** `call::Own` holds the user and `Welcome.conn_id`, and
`Own::is` matches the connection, or the user when the server sends no
connection id, as `load::Session::own_track` does. `note_track` and
`track_update` use it, so a second session of the account is a remote track
the first decodes. The spike gives every session a new user id, so the test
uses a scripted server that, like the real one, gives an account one id. On
the real server one seat per user (T-086) means the second join takes the
first session's seat, but `ss-cli connect` only talks to the spike (it logs
in with no password).

**CLI closing quantum.** `VoiceGraph::push_capture` puts a closed quantum in
as silence, so nothing the microphone hears after the gate closes is sent; a
measurement click on a closed gate is mixed into silence. A frame that holds
an open quantum is sent even when its last quantum closed the gate, with
`TALK_START | TALK_END` from idle. The leading-silence clock advance and
pre-roll retraction of `ss-client-core` are not ported: the CLI's VAD has no
pre-roll.

**Resync and `Retry-After`.** `History::page` turns a `429` into
`Error::Server { RATE_LIMITED, retry_after }`, read from `Retry-After` in
whole seconds. `load_older`, `load_newer` and `load_around` get the same
error, which the desktop shows as `server_5` with "history rate limited"
instead of an HTTP error. The resync's `resync_page` waits `stated_delay`
(the wait plus up to 25 %, at most 10 minutes; one second when the header is
missing) and asks again, at most three times per page, then defers the
channel as before. `02-protocol.md` §1.2 says so. The test front in
`client_core.rs` (`https_with`, `Throttle`) refuses the first history
request with `Retry-After: 1`; the test asserts the retry came at least 1 s
and under 3 s after the refusal.

**Mention double count: real, and not fixable in the client.** The test
signs in twelve sessions of the owner while four accounts send twenty
mentions. Every session ends 1 to 4 above the count a later session reads
from the database. The server subscribes to the bus before it builds the
snapshot (`connection.rs`, `self.bus.subscribe()` before `prepare_welcome`),
so a mention committed in between is in `mention_count` and arrives as a
live `MessageCreated`, which the client counts. The client cannot tell
those events from later ones: guild channels have no `last_message_id` in
the snapshot, there is no read-state query, and ULIDs are not monotonic.
A fix belongs on the server. Options: publish `MessageCreated` while still
holding the writer and build the read states under it, or put a message
horizon (rowid or id) in the snapshot, which is a protocol change. The test
is `#[ignore]` with that reason; it passes once the server is fixed.

**Scroll position: left to T-108.** When the four-page budget runs out, the
core drops older cached messages and their coverage (T-089, T-096
`a_resync_that_drops_older_messages_forgets_their_coverage`). Whether the
open list jumps is the desktop's list rendering; nothing in the core can
observe it.

**Files outside the Paths**
- `crates/ss-net/tests/loopback.rs`: the two router tests; the old
  `event_overflow_closes_instead_of_silently_losing_state` asserted the
  close and is replaced.
- `crates/ss-client-core/src/connection/pump_tests.rs` (`connection.rs`'s
  test module): the burst test, and a comment in the T-081 test.
- `crates/ss-server/tests/client_core.rs`: `https_with` and `Throttle`
  (`https` calls it), the resync test, the ignored mention test. The webhook
  test is untouched.
- `docs/plan/02-protocol.md`: §1.2 resync `429`, §2 router backpressure.
- `docs/plan/tasks/INDEX.md`: status.

No `.proto` change. The `FrameDecoder` is unchanged, so no fuzz build.

**Tests.** `just check` passes. `cargo clippy -p socialspeak-desktop
--all-targets -- -D warnings` passes. The pump tests, burst included,
passed 5 runs of 5; the resync test 3 of 3.

**Open questions**
- The mention double count needs a server task (T-109 owns `snapshot.rs`,
  but the subscription order is in the server's `connection.rs`).
- `load::Session` could read events while it awaits `JoinVoice` and
  `LeaveVoice`, as `join_client` now does, if load runs put more than 256
  events in front of one Ack.

