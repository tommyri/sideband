# ss-net

QUIC (`quinn`) connection setup for both ends, TLS with trust-on-first-use
certificate pinning, the framed control stream, and the audio datagram
header. The server and the client core both depend on this crate.

## May depend on

`ss-core`, `ss-proto`, `quinn`, `rustls` 0.23, `tokio`, `bytes`, and `prost`.
`rcgen`, `sha2`, and `webpki-roots` implement certificate trust; `serde`,
`serde_json`, `tempfile`, and `parking_lot` support persistent pin storage.
`thiserror` defines boundary errors; `proptest` is test-only.

## Must not depend on

`ss-audio`, `ss-input`, `ss-client-core`, `ss-server`, `ss-cli`. No database
and no permission checks.

## Use

Server: await `tls::generate_self_signed(data_dir)`, then
`server::Endpoint::bind(addr, tls)`. Dispatch each `accept()` result into a
bounded task for `Incoming::accept()`, then `Connection::accept_control()`.
Client: `client::connect(host, port, Arc<dyn PinStore>)`, then
`open_control()`. First-use rejection exposes a fingerprint; explicit user
approval calls `PinStore::set` before retrying. Public WebPKI certificates
are accepted and pinned automatically; changed pins always fail.

`ControlStream::send/recv` retain partial frames across cancellation.
`ClientControl::start` adds bounded request correlation and an event receiver;
use `request` for Hello/Welcome or Ping/Pong and `call` for Ack/Error.
Send Hello first; only then does the router send idle Pings every 15 seconds.
Dropping it
closes the connection. Request timeouts and reconnect policy belong above us.

Datagram parsing borrows the payload. QUIC does not guarantee delivery or
order; callers use the header sequence number. `StreamHeader` and raw QUIC
uni-stream access reserve the phase-5 video path without implementing video.
Pin-store filesystem methods are synchronous: call them on a blocking thread
and share one JSON store instance per file. `connect` does this internally.
