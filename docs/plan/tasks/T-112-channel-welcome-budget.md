# T-112 — Cap channels so one member cannot fill the Welcome

Status: todo
Phase: 4
Size: S
Depends on: T-109
Paths: `crates/ss-server/src/limits.rs`, `crates/ss-server/src/handlers/`, `crates/ss-server/tests/`, `docs/security.md`, `docs/plan/03-data-model.md`

## Goal
No single member can make every `Hello` fail by creating content, so the
Welcome bound in `docs/security.md` §7 holds against a hostile member with
channel rights.

Source: T-109. It capped roles at 250 and measured the Welcome: a guild
channel with a full topic costs about 4,440 bytes, so about 230 such
channels fill the 1 MiB frame on their own. Anyone with `MANAGE_CHANNELS`
can create them, and then every member's `Hello` is refused.

## Scope
- Cap guild channels, or budget their topic bytes, or both, so that at the
  caps (with roles at 250) the longest-string Welcome still leaves room for
  members. Pick the numbers from T-109's table in `tests/welcome_bound.rs`
  and state them in `docs/security.md` §7.
- Refuse creation past the cap with a clear error, inside the write
  transaction, as `CreateRole` does.
- Decide whether existing servers over the cap keep their channels (refuse
  only new ones) and record it.

## Out of scope
Member paging, which would be a protocol change.

## Acceptance criteria
- Test: creating a channel past the cap is refused.
- `welcome_bound` measures a Welcome at the channel and role caps with
  every string at its longest and asserts the stated member headroom.

## Notes from implementation
