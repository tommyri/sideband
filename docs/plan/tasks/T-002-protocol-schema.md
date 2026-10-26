# T-002 — Protocol schema v1 and `ss-proto`

Status: todo
Phase: 0
Size: M
Depends on: T-001
Paths: `crates/ss-proto/proto/socialspeak/v1/*.proto`, `crates/ss-proto/src/`

## Goal
The protobuf schema from `02-protocol.md` §3 compiled with `prost`, plus the
length-prefixed framing codec and protocol-version negotiation helpers.

## Scope
- `.proto` files split by area: `common.proto` (Id, Timestamp, Error codes,
  Permissions as u64), `auth.proto`, `channels.proto`, `messages.proto`,
  `voice.proto`, `roles.proto`, `moderation.proto`, `envelope.proto`.
- `build.rs` with `prost-build`; generated code committed? **No**: generate at
  build time, but vendor `protoc` via `protoc-bin-vendored` so CI needs no
  system protoc.
- `Framed` codec: `encode(&ServerEnvelope) -> Bytes`, async
  `read_frame(&mut quinn::RecvStream) -> Result<ClientEnvelope>` with the
  1 MiB cap, and symmetric for the other direction. Keep the codec generic over
  `AsyncRead/AsyncWrite` so tests use in-memory duplex.
- `PROTOCOL_VERSION: u32 = 1`, `SUPPORTED_RANGE`.
- Convenience constructors: `ServerEnvelope::ack(request_id, result)`,
  `ServerEnvelope::error(request_id, code, msg)`.
- Conversions `ss_core::Id <-> bytes`, `Permissions <-> u64` with validation.

## Out of scope
Datagram header (T-004), any business logic.

## Acceptance criteria
- Every message in `02-protocol.md` §3 exists with the listed fields.
- Round-trip `proptest` for framing (random envelopes → encode → decode).
- Oversized frame returns `FrameError::TooLarge`, never panics.
- `cargo doc` has a module-level doc linking to `02-protocol.md`.

## Notes / references
- Use `bytes::Bytes` for payload fields to avoid copies.
- Keep `oneof` names stable; they become JSON keys in the bot API later.

## Notes from implementation
