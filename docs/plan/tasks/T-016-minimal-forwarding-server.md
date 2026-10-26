# T-016 — Minimal voice forwarding server (spike)

Status: in-progress
Phase: 1
Size: S
Depends on: T-004
Paths: `crates/ss-server/`, `crates/ss-net/` (transport performance), workspace build config

## Goal
A server that accepts QUIC connections, lets a client "join channel N" over
the control stream, and forwards audio datagrams to the other members of that
channel. This is the seed of the real media router.

## Scope
- Binary `ss-server` with `--data-dir`, `--bind`; generates a self-signed
  cert on first run and prints its fingerprint.
- Accept `Hello` with any username (no auth yet), reply `Welcome` with an
  empty snapshot.
- Handle `JoinVoice{channel_id}` / `LeaveVoice`; assign `track_id`s; send
  `TrackAdded/TrackRemoved` and `VoiceStateUpdated` to channel members.
- Datagram path: parse header, validate track ownership, fan out to channel
  members via `send_datagram`; count drops.
- Structured as `media::Router` and `voice::Registry` modules that T-025 will
  keep.

## Out of scope
Auth, persistence, text, permissions.

## Acceptance criteria
- Integration test: 3 in-process clients join the same channel; datagrams
  from A reach B and C and not A; a 4th client in another channel receives
  nothing.
- Forwarding path does no allocation per datagram (reuse `Bytes` slice).
- 20 clients × 50 pps sustained on a laptop with < 5 % of one core
  (rough check, numbers in notes). The user explicitly accepted the measured
  5.08% result on 2026-10-26.

## Notes from implementation

- Implements the reusable `voice::Registry` and `media::Router`, with a thin
  CLI (`--data-dir`, `--bind`), persistent TLS identity/fingerprint, and graceful
  Ctrl-C shutdown. Payloads stay opaque and share Bytes without per-forward
  allocation. Counters distinguish rejected input, forwarded copies, and send
  drops; QUIC/network loss is not included in application drop counts.
- Hello accepts Login/Register usernames without authenticating passwords or
  invites, assigns fresh user/connection ids, and returns an empty snapshot.
  Oversized Welcome responses receive INVALID before a session is registered.
  Channel ids use shared ss-proto/ss-core validation. Accounts, persistence,
  permissions, token/resume authentication, and channel catalogs remain deferred.
- Join announces existing tracks/states to the newcomer and its assignment to
  channel members. Switching leaves and joins atomically; same-channel rejoin
  updates flags without replacing the track. Leave/disconnect remove tracks.
  Track ids are never reused during one server lifetime. Mono 48 kHz Opus is
  advertised; profile-derived metadata remains T-025. Ping/Pong and local
  mute/deaf support usable voice sessions and transport keepalive.
- Limits: 256 pending/active sessions, 10-second handshake/open deadlines,
  and 516 outgoing control slots per peer. This fits the largest supported
  bootstrap plus switch/reply headroom. Overflow still closes slow consumers.
- Review fixed two pre-existing T-016 issues: the healthy 128th join overflowed
  the original control queue, and a valid large Hello expanded into a Welcome
  over the 1 MiB limit. Both were reproduced with failing tests before fixing;
  regressions now cover a full 256-member channel and an explicit INVALID
  response for a 600,000-byte username without a ghost session.
- Performance diagnosis found UDP packet processing dominated CPU. Enabled
  Quinn's macOS batched socket path and automatic path-MTU discovery up to an
  8192-byte receive ceiling on both endpoints. Initial/minimum MTU stays 1200;
  successful probes, peer limits, and black-hole recovery control increases.
  Audio frames stay capped at 1150 bytes. No timer delays or reduced workload
  were introduced. Client IPv6 dual-stack behavior is preserved with socket2;
  smaller-peer MTU and IPv4-mapped IPv6 regressions cover compatibility.
- The CLI uses one Tokio runtime thread and yields once before routing ready
  media. Release builds use thin LTO and one codegen unit. The no-allocation
  regression measures forwarding a freshly received QUIC Bytes buffer.
- Accepted CPU result: Apple M4 Pro, macOS 27.0.1, release binary, 20 loopback
  clients each sending 50 packets/s on a synchronized 20 ms cadence for 30 s,
  200-byte payloads (80 kbit/s equivalent), one channel. All 30,000 inputs were
  accepted and all 570,000 recipient datagrams arrived, with zero drops/rejects.
  Server-only OS process CPU was 0.7934 s user + 0.7313 s system = 1.5247 s,
  or **5.08% of one core** over the 30-second workload, including startup and
  shutdown CPU. The user explicitly accepted 5.08% instead of the original
  <5% target. Previous same-workload baseline was 7.98% (profiling run).
  This is a rough local measurement on a path that discovered an 8192-byte
  MTU, not a guarantee for Ethernet-MTU, differently paced, or remote traffic.
- Reproduce load with `cargo test --release -p ss-server --test voice_spike
  sustained_twenty_clients -- --ignored --nocapture`. For server-only CPU,
  run the release server separately, set `SS_LOAD_PORT` and
  `SS_LOAD_FINGERPRINT` for the test process, then measure the server process's
  user+system CPU with an OS timer (`wait4`/`time`). The in-process test includes
  client CPU and must not be reported as server CPU. Temporary profiling code
  was removed; the packet rate, payload size, and recipient count are unchanged.
- Integration tests drive ss-net directly because ss-client-core is still a
  scaffold and persistence is outside this spike. Existing design plans and
  task worktrees are preserved. Linux/Windows execution was not available;
  the performance result and runtime tests are macOS-specific.
- Final validation: `just check` passed (114 tests, one manual load test ignored
  in the ordinary suite); the release load test delivered all 570,000 copies.
  `just check-deps`, warning-denied ss-net/ss-server rustdoc, the final release
  build, and `git diff --check` passed. Refreshed both workspace and fuzz lockfiles
  for the direct transport dependencies. Review has no unresolved blockers.
