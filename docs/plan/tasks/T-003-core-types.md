# T-003 — `ss-core`: ids, permissions, config, errors

Status: todo
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
