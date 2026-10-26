# T-004 — `ss-net`: QUIC endpoint, TLS/TOFU, control stream, datagram header

Status: todo
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
