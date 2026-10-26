# ss-core

Ids, permission bitflags, channel and user enums, voice profiles, timestamps,
and the error types shared by every other crate.

## May depend on

Nothing else in this workspace. Third-party crates limited to data types
(`ulid`, `bitflags`, `serde`, `thiserror`). The optional `sqlx` feature adds
a 16-byte `BLOB` mapping for `Id` and is off by default. No sockets, no audio,
and no async runtime unless that feature is enabled.

## Must not depend on

`ss-proto`, `ss-net`, `ss-audio`, `ss-input`, `ss-client-core`, `ss-server`,
`ss-cli`.
