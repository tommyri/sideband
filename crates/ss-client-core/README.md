# ss-client-core

The client without a user interface. It owns the QUIC connection, applies
server events to an in-memory state store, and runs the voice session
(devices, encoder, jitter buffers, mixer) while a voice channel is joined.

`ss-cli` and the Tauri shell both call into this crate. Neither of them
reimplements protocol or audio logic.

## May depend on

`ss-core`, `ss-net`, `ss-audio`, `ss-input`.

## Must not depend on

`ss-server`, `ss-cli`. No SQL and no windowing toolkit.
