# ss-core

Ids, permission bitflags, channel and user enums, timestamps, and the error
types shared by every other crate.

## May depend on

Nothing else in this workspace. Third-party crates limited to data types
(`ulid`, `bitflags`, `serde`, `thiserror`). No async runtime, no sockets,
no audio.

## Must not depend on

`ss-proto`, `ss-net`, `ss-audio`, `ss-input`, `ss-client-core`, `ss-server`,
`ss-cli`.
