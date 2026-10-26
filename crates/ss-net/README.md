# ss-net

QUIC (`quinn`) connection setup, TLS with trust-on-first-use certificate
pinning, the framed control stream, and media headers. Both peers use it.

May depend on `ss-core`, `ss-proto`, `quinn`, `quinn-udp`, `rustls` 0.23,
`tokio`, `bytes`, `prost`, `socket2`, and certificate/pin storage helpers.
Must not depend on `ss-audio`, `ss-input`, `ss-client-core`, `ss-server`, or
`ss-cli`. No database or permission checks.

Server: await `tls::generate_self_signed(data_dir)`, then
`server::Endpoint::bind(addr, tls)`. Dispatch each `accept()` into a bounded
peer task for `Incoming::accept()`, then `Connection::accept_control()`.
Client: `client::connect(host, port, Arc<dyn PinStore>)`, then `open_control()`.
First-use rejection exposes a fingerprint; explicit approval calls
`PinStore::set` before retrying. Public WebPKI certificates are accepted and
pinned automatically; changed pins always fail.

`ControlStream::send/recv` retain partial frames across cancellation.
`ClientControl::start` adds bounded request correlation and an event receiver;
use `request` for Hello/Welcome or Ping/Pong and `call` for Ack/Error.
Send Hello first; only then does the router send idle Pings every 15 seconds.
Dropping it closes the connection. Reconnect policy belongs above us.

Datagram parsing borrows the payload. Delivery is unreliable and unordered;
clients use the sequence number. `StreamHeader` and raw QUIC uni-stream
access reserve the video path. Audio payloads remain capped at 1150 bytes.

Endpoints advertise an 8192-byte UDP receive ceiling. Path-MTU discovery
starts at 1200 bytes and grows only after successful probes, respecting peer
limits and retaining black-hole recovery. Capable paths can combine more
frames without an application timer; ordinary MTU paths remain supported.
macOS enables Quinn's `fast-apple-datapath` for batched socket I/O. Other
platforms keep their native Quinn paths. Clients preserve dual-stack binding.

Pin-store filesystem methods are synchronous: use a blocking thread and share
one JSON store instance per file. `connect` does this internally.
