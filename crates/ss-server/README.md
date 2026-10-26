# ss-server

The server owns control dispatch, voice membership (`voice::Registry`), and
opaque audio fan-out (`media::Router`). Media payloads are never decoded.
T-016 is an unauthenticated spike: any Login/Register username gets a fresh
user and an empty snapshot. Channel ids are arbitrary 16-byte ids; accounts,
persistence, permissions, and session resumption arrive in phase 2.

Run `cargo run -p ss-server -- --data-dir ./data --bind 127.0.0.1:4433`.
The first run creates a persistent TLS identity and prints its fingerprint.
Ctrl-C closes connections and reports media counters. The spike uses one
Tokio runtime thread; blocking identity-file work is offloaded by ss-net.

One audio track is assigned per joined connection. Joins announce existing
tracks to the newcomer and its track to all channel members. Leave, switch,
and disconnect remove the old assignment. Mute/deaf flags affect forwarding.
The spike advertises mono 48 kHz Opus; profile negotiation is future work.

Sessions and outgoing control queues are bounded; a full 256-member bootstrap
fits before the writer resumes. Oversized Welcome replies return INVALID. Slow control consumers are
closed instead of losing track events. The media path reuses Bytes, validates
ownership, skips the sender and deaf peers, and counts invalid input and send
drops. It drops new packets when Quinn's bounded send queue is full.

`cargo test -p ss-server` covers real loopback QUIC clients and zero steady
forwarding allocations. Run the ignored `sustained_twenty_clients` test with
`--release -- --ignored --nocapture` for a 30-second, 20-client load check.
Release builds use thin LTO and one codegen unit. The measured server CPU was
5.08% of one core on an M4 Pro with an 8192-byte loopback PMTU.
To target a separate loopback server for CPU measurement, set `SS_LOAD_PORT`
and `SS_LOAD_FINGERPRINT` to its printed port and SHA-256 certificate pin.

May depend on `ss-core`, `ss-proto`, `ss-net`, Tokio, Quinn, and server-side
storage/HTTP/logging/CLI libraries. Must not depend on `ss-audio`, `ss-input`,
`ss-client-core`, or `ss-cli`.
