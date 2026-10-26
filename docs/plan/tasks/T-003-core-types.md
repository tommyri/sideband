# T-003 — `ss-core`: ids, permissions, config, errors

Status: done
Phase: 0
Size: S
Depends on: T-001
Paths: `crates/ss-core/src/`

## Goal
Shared, dependency-light domain primitives used by every other crate.

## Scope
- `Id`: newtype over `ulid::Ulid` with `Display`/`FromStr` (Crockford base32),
  `as_bytes()`, serde as string, `sqlx::Type` as BLOB (behind feature `sqlx`).
- `Permissions`: `bitflags! u64` exactly as `03-data-model.md`; `resolve()`
  helper implementing the base/override algorithm given iterators of role
  perms and overrides; `hierarchy_allows(actor_pos, target_pos)`.
- `ChannelKind`, `UserKind`, `VoiceProfile` enums with `#[non_exhaustive]`.
- `VoiceProfile::encoder_params()` returning bitrate range/default, channels,
  application, frame size (table in `02-protocol.md` §4).
- `Timestamp` helpers (ms since epoch, `now()`).
- `Error` conventions: a `CoreError` for parsing only; no I/O here.
- Default `@everyone` and `Admin` permission constants.

## Out of scope
Anything async, networking, or storage.

## Acceptance criteria
- `Permissions::resolve` has table-driven tests covering: admin bypass, deny
  then allow precedence, multiple roles, DM channel rule.
- `Id` round-trips through string and bytes; invalid input errors cleanly.
- Crate has zero dependencies on tokio/quinn/sqlx by default.

## Notes from implementation

- `Permissions::resolve` takes the role iterator, the override iterator, a
  `ChannelKind`, and `is_member`. `@everyone` is not inserted implicitly;
  the caller passes it in the role iterator (OR is idempotent with the
  base formula). Overrides zip with roles in iterator order. Roles without
  an override are unchanged by the override pass. Overrides past the last
  role are ignored.
- `ADMINISTRATOR` in that OR returns `Permissions::all()` (defined bits
  0–18, not every `u64` bit) and skips overrides. An override that only
  grants `ADMINISTRATOR` sets that bit and does not expand to every other
  flag, matching the single check before the override loop.
- `hierarchy_allows` is strict `actor_pos > target_pos` on `i64` role
  positions. The owner short-circuit stays in the server (T-022).
- `ChannelKind::Dm` and `ChannelKind::GroupDm` both use the membership
  rule (T-028): roles and overrides are ignored. Members get
  `VIEW_CHANNEL | SEND_MESSAGES | ATTACH_FILES` (`Permissions::DIRECT_MESSAGE`).
  Non-members get none. `is_member` is ignored for other kinds.
- Bitrates on `EncoderParams` are decimal bits per second (1 kb/s = 1000).
  `low_latency` has no range in `02-protocol.md` §4, so min and max are
  both `64_000`. The struct also carries in-band FEC and expected packet
  loss from that table. Loss is `Some(5)` for `voice` only; `music` and
  `low_latency` say FEC is on without a percent, so the field is `None`.
  `contains_bitrate` is inclusive.
- The `sqlx` feature implements `Type`, `Encode`, and `Decode` for `Id`
  as a 16-byte `BLOB`. It uses the workspace `sqlx` dependency, whose
  features include `runtime-tokio`, so enabling it also links tokio.
  The default build does not depend on tokio, quinn, or sqlx.
- `Id::as_bytes` returns `[u8; 16]` by value. `Timestamp::now` yields `0`
  if the clock is before the Unix epoch and `i64::MAX` if the millisecond
  count does not fit in `i64`.
