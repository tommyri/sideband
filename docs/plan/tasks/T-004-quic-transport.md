# T-004 — `ss-net`: QUIC endpoint, TLS/TOFU, control stream, datagram header

Status: in-progress
Phase: 0
Size: M
Depends on: T-002, T-003
Paths: `crates/ss-net/src/`

## Goal
Everything needed to open a SocialSpeak QUIC connection from either side, run
the control stream, and encode/decode media datagram headers. No business
logic.

## Scope
- `server::Endpoint::bind(addr, TlsConfig)`: quinn server config with ALPN
  `socialspeak/1`, datagrams enabled, idle timeout 30 s, keep-alive off
  (client pings), `max_concurrent_uni_streams` sized for video later.
- `client::connect(host, port, PinStore)`: rustls verifier that pins the
  server certificate by SHA-256 fingerprint. First connect returns
  `Err(Untrusted { fingerprint })` so the caller can ask the user; `PinStore`
  trait with an in-memory and a JSON-file implementation.
- `tls::generate_self_signed(data_dir)` using `rcgen`; persisted key/cert;
  `fingerprint()`.
- `ControlStream`: wraps the bi stream with `ss-proto` framing; `send(msg)`,
  `recv() -> msg`; request-id allocator on the client side with
  `call(msg) -> Future<Ack|Error>` correlating responses, and a separate event
  receiver for `request_id == 0` messages.
- `media::DatagramHeader { version, kind, flags, track_id, seq, timestamp }`
  with `encode_into(&mut BytesMut)`, `parse(&[u8]) -> (Header, payload)`.
  Zero-copy parse (returns slice offset).
- `media::StreamHeader` for phase 5 (defined, tested, unused).
- `ConnectionStats` accessor (RTT, lost packets, congestion window) from quinn
  for the diagnostics panel.

## Out of scope
Reconnect policy (client-core), auth, forwarding.

## Acceptance criteria
- Integration test: server + client in one process over loopback: TOFU
  rejection → pin → connect → exchange `Ping/Pong` → send 1000 datagrams and
  receive them in order with parsed headers.
- `proptest` round-trip for `DatagramHeader`; malformed input never panics.
- Fingerprint change after pinning is rejected with a distinct error.

## Notes / references
- quinn datagram max size depends on path MTU; expose `max_datagram_size()`.
- Set `transport.datagram_receive_buffer_size` generously (e.g. 4 MiB) on the
  server.

## Notes from implementation

- Reviewed and validated locally on macOS with Rust 1.99.1: `just check`
  (105 tests), `just check-deps`, and `cargo doc -p ss-net --no-deps` with
  rustdoc warnings denied. Windows/Linux execution remains for CI or those
  hosts. The nightly AddressSanitizer fuzz smoke run completed 562,829 cases
  in 31 seconds without failures.
- Endpoint configuration uses ALPN `socialspeak/1`, TLS 1.3 with rustls 0.23
  and an explicit ring provider, 30-second idle timeout, no QUIC keepalive,
  4 MiB datagram send/receive buffers, one client-initiated bi stream, and
  64 concurrent incoming uni streams for future video. `Endpoint::accept`
  returns a pending `Incoming`; callers spawn bounded tasks for its handshake
  and can refuse peers at capacity. Incomplete peers do not block acceptance
  of others. Both endpoints reject peers that do not negotiate datagrams.
  No auth or forwarding is implemented.
- `client::connect` accepts an `Arc<dyn PinStore>`, resolves the original
  host, and tries its addresses, including after local socket-family or
  synchronous connect setup errors. Trust failures remain terminal.
  Pins are scoped to canonical host + port.
  Unknown certificates return `Untrusted`; changed pins return
  `FingerprintChanged`, including CA-valid replacements. TLS handshake
  signatures are always checked. Mozilla WebPKI roots accept and record
  unpinned public certificates; `connect_with_roots` permits private roots.
- `MemoryPinStore` and `JsonPinStore` serialize updates. `set` is explicit
  approval/replacement; `remember` refuses a conflicting pin. JSON updates
  use a synced temporary file and atomic replacement, updating memory only
  after persistence succeeds. Share one store instance per file; concurrent
  writers in different processes are not supported. Synchronous filesystem
  operations run on the blocking pool during connection setup.
- The generated DER certificate and PKCS#8 key are persisted as one atomic
  `data_dir/tls/identity.json` pair (0600 on Unix). Concurrent first starts
  use the installed identity. Corrupt or mismatched pairs fail rather than
  rotate. `generate_self_signed` is async and offloads filesystem/crypto work.
- `ControlStream<Out, In>` uses `ss-proto` encoding/decoding and keeps partial
  reads/writes across cancellation. The 1 MiB cap is checked before reading
  a body. A framing failure closes the connection. A locally oversized send
  leaves it usable. Split readers/writers support independent tasks.
- `ClientControl` correlates replies, checks duplicated Ack/Error request ids,
  and exposes an event receiver for id zero. `request` supports Hello/Welcome
  and Ping/Pong; `call` returns `Reply::Ack` or `Reply::Error`. The allocator
  continues after raw sends, never reuses cancelled ids, and closes on u32
  exhaustion. A 64-command queue, 256 pending calls, and 256 events bound
  memory. Event overflow closes the connection rather than losing state;
  dropping the router fails pending calls and stops both workers. Caller
  timeouts and reconnect policy stay above this crate. Idle outbound control
  sends Ping every 15 seconds after the application sends its initial Hello;
  a fresh router stays silent. The server application replies with Pong.
- Protocol §4 now specifies canonical unsigned LEB128 for the previously
  ambiguous 1–3 byte track id (0..=0x1f_ffff). QUIC varints cannot express
  that width table. Version, kind, flags, noncanonical/overlong ids, truncated
  headers, and audio payloads over 1150 bytes are rejected. Parsing borrows
  the payload. `StreamHeader` uses kind one and a big-endian group id, with
  no video framing/codec implementation yet. D2 and protocol §4–§5 were
  clarified together; codec settings did not change.
- QUIC datagrams are unreliable and unordered. The 1000-datagram loopback
  acceptance test uses one packet in flight, verifies parsed sequence numbers
  and unchanged payloads, and echoes each packet before sending the next.
  This demonstrates the wire path without promising transport ordering.
- Tests cover TOFU rejection/approval, changed pins, private-root WebPKI,
  ALPN mismatch, missing datagram support, request correlation/cancellation,
  event overflow, partial-frame cancellation, oversized-prefix rejection,
  idle Ping, uni streams, concurrent identity creation, persistence failure,
  exact media bytes, round-trip properties, and arbitrary malformed media.

- Review fixes: gated automatic Ping until initial application traffic;
  continued address fallback after local setup errors; separated incoming
  acceptance from handshake completion; added `fuzz/media_headers` per the
  testing conventions. Regression tests reproduced the first two failures
  before the fixes, and a real incomplete QUIC Initial verifies independent
  acceptance of another peer. The fuzz harness covers both media parsers and
  exact round trips, isolated from normal workspace builds. Sustained fuzzing
  and CI integration remain T-061. Follow-up review found no remaining
  actionable issues in the fixes.
