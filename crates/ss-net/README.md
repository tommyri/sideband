# ss-net

QUIC (`quinn`) connection setup for both ends, TLS with trust-on-first-use
certificate pinning, the framed control stream, and the audio datagram
header. The server and the client core both depend on this crate.

## May depend on

`ss-core`, `ss-proto`, plus `quinn`, `rustls`, `tokio`, and `bytes`.

## Must not depend on

`ss-audio`, `ss-input`, `ss-client-core`, `ss-server`, `ss-cli`. No database
and no permission checks.
