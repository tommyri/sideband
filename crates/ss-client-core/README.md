# ss-client-core

The client without a user interface. It owns the QUIC connection, applies
server events to an in-memory state store, and runs the voice session
(devices, encoder, jitter buffers, mixer) while a voice channel is joined.

`ss-cli` and the Tauri shell both call into this crate. Neither of them
reimplements protocol or audio logic.

`media::VideoTrackSender` opens byte-level GOP writers after StartStream.
One `VideoTrackReceiver` accepts all incoming video groups and exposes their
track ids for demux. Subscribe through control first; reset/drop obsolete GOPs.
No capture, codec, or rendering is part of these APIs.

## May depend on

`ss-core`, `ss-net`, `ss-audio`, `ss-input`, and Quinn stream handles.

## Must not depend on

`ss-server`, `ss-cli`. No SQL and no windowing toolkit.
