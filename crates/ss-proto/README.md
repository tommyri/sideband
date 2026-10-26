# ss-proto

Control-message schema for SocialSpeak protocol v1. `.proto` files live under
`proto/socialspeak/v1/`. `prost` types are generated at build time, along with
the length-prefixed framing codec and protocol-version helpers. The schema is
the public API that bots will use later. Wire rules:
`docs/plan/02-protocol.md`.

## May depend on

`ss-core`, `prost`, `bytes`, `thiserror`, and `tokio` (the async frame reader
and writer). `prost-build` and `protoc-bin-vendored` are build-only, so CI
does not need a system `protoc`. Generated Rust is not committed.

## Must not depend on

`ss-net`, `ss-audio`, `ss-input`, `ss-client-core`, `ss-server`, `ss-cli`.
No sockets and no business logic.

`ss_core::Id` and `ss_core::Permissions` are not wired here. Those types land
in T-003. Until then, validate 16-byte ids and permission bitsets with
`validate_id` and `validate_permissions`.
