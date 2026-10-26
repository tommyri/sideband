# T-002 — Protocol schema v1 and `ss-proto`

Status: done
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

- `Id` and `Timestamp` exist in `common.proto`, but entity fields use raw
  `bytes` and `int64` as `02-protocol.md` requires. `validate_id` /
  `validate_optional_id` check the 16-byte ULID length. There is no
  conversion to `ss_core::Id`; that type does not exist yet.
- Permissions stay a `uint64` on the wire (`Role.permissions`, override
  `allow` / `deny`), with bit constants and `validate_permissions` in
  `ss_proto::validate`. Unknown bits above bit 18 are rejected. Wiring to
  `ss_core::Permissions` waits for T-003. A nested `Permissions` message
  would have made the field length-delimited instead of a u64 scalar.
- Proto3 enums reserve 0 as unspecified. Those numbers are not the SQL
  integer codes in `03-data-model.md`.
- `Error` carries optional `min_version` and `max_version` for section 7.
  `ServerEnvelope::error` leaves them unset. `unsupported_version` fills
  them from `SUPPORTED_RANGE` (`1..=1`).
- `Ack` and `Error` repeat `request_id` inside the payload because the
  section 3 table lists it, and the envelope also carries it. The
  constructors set both copies to the same value.
- Messages the section 3 table only names (`User`, `Channel`, `Message`,
  `Role`, `Invite`, `Snapshot`, `VoiceSettings`, `Codec`, `Member`, and
  the member/presence events) are defined so the listed events have
  fields. Extra snapshot fields come from `03-data-model.md` (attachment
  metadata, `Role.is_default`, DM `member_ids`, webhook id). Password
  hashes, token hashes, and storage paths stay off the wire.
  `Ack.result` adds `role` and `user` beside the examples in the table.
- `track_id` is `uint32`, not a ULID. Section 4 assigns it as a small
  varint per connection. `conn_id` is a server-assigned 16-byte ULID, not
  the QUIC connection id.
- `MemberJoined`, `MemberLeft`, `MemberUpdated`, and `PresenceUpdated`
  live in `envelope.proto` (no members file in the task split).
  `MoveUser` and `ServerMuteUser` live in `moderation.proto`. Video
  messages (`SubscribeTrack`, `UnsubscribeTrack`, `RequestKeyframe`,
  `SetPreferredQuality`) live in `voice.proto`.
- Framing is generic over `prost::Message` and `tokio::io::{AsyncRead,
  AsyncWrite}` (`encode`, `decode`, `read_frame`, `write_frame`), not
  `quinn` streams and not `FramedRead` / `FramedWrite` wrapper types, so
  tests use an in-memory duplex and this crate does not depend on
  `quinn`. After `FrameError::TooLarge` the payload is still unread; the
  caller drops the connection.
- `tokio` is a normal dependency (workspace features, unchanged).
  `prost-build` and `protoc-bin-vendored` are build-only. Generated Rust
  is not committed. Generated oneofs allow `clippy::large_enum_variant`
  and `clippy::missing_const_for_fn`; boxing them would change the match
  types.
