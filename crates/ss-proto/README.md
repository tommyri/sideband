# ss-proto

Control-message schema for Sideband protocol v1. `.proto` files live under
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

`validate` converts raw wire ids with `id_from_bytes` / `id_to_bytes` and
permission masks with `permissions_from_bits` / `Permissions::bits`.
`v1::Id` also supports `From<ss_core::Id>` and checked conversion back.
`validate_id` and `validate_permissions` use these core-backed conversions;
unknown bits are rejected, and permission validation never expands roles.
