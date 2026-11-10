# T-093 — Session revocation, logout and expiry

Status: in-progress
Phase: 4
Size: S
Depends on: T-021, T-061
Paths: `crates/ss-server/src/auth/`, `crates/ss-server/src/db/sessions.rs`, `crates/ss-server/src/main.rs`, `crates/ss-proto/proto/socialspeak/v1/`, `crates/ss-client-core/src/`, `apps/desktop/src-tauri/src/onboarding.rs`, `docs/security.md`, `docs/plan/02-protocol.md`

## Goal
A stolen token can be made useless without banning the victim or editing
the database.

Source: 2026-11-09 review at `d861a0a`. Traced by reading the code, not
reproduced.

## Scope
- `auth::set_password` updates the hash and leaves every session intact;
  `sessions::delete` is called only by its own test; the protocol has no
  logout; expiry slides on every use, so a token used once every 90 days
  never expires. Delete the user's sessions in `set_password`.
- Add a `Logout` command (new tags only) that revokes the current session,
  and call it from "remove server" and "sign out" in the desktop app.
  Removing a server today forgets the token locally only.
- Add `ss-server user sessions list|revoke` for the operator, and let a
  user list and revoke their own sessions if that fits in the size.
- Prune expired rows on a timer. Decide on an absolute lifetime beside the
  sliding one and record it in `docs/security.md` section 1.
- `Welcome.resume_token` is the 90-day session token itself and doubles as
  the REST bearer, while T-060 calls it short-lived. Either issue a
  separate short-lived resume token or correct the documents.
- Login timing: an unknown username skips argon2. Verify against a dummy
  hash so the two cases take the same time.
- Kicking should end the kicked session's token, or the documents should
  say a kick is only a disconnect. Check and record which.

## Out of scope
Multi-factor authentication. Token storage in the OS keychain on the
client; note it in `BACKLOG.md` if not already there.

## Acceptance criteria
- Tests: a token stops working on QUIC and HTTPS after a password reset,
  after `Logout` and after the operator revokes it; connected sessions are
  closed.
- Test: login for an unknown username runs the hash.
- Expired sessions are removed by the pruner.
- `docs/security.md` and `02-protocol.md` describe the token lifetimes
  that the code implements.

## Notes from implementation

Implemented (2026-11-10).

Findings, each confirmed by a test that failed first:
- Password reset kept sessions: after `auth::set_password` the old token
  still passed `Hello` (`Ok(Authenticated { .. })`).
- No logout: `Logout` (proto added first) was answered `INVALID`
  "unsupported command".
- Sliding expiry never ended: a session created 400 days ago, last used 89
  days ago, still passed `Hello` and slid again.
- No pruning: `sessions::delete` was called only by its own unit test
  (`git grep`), so there was no failing test to write; the pruner test was
  written against the new API.
- Login timing: the unit test armed the argon2 probe and an unknown username
  returned without reaching it (5 s timeout).
- `Welcome.resume_token` is the 90-day session token and the HTTPS bearer;
  T-060 calls it short-lived. Correct. I fixed the documents, not the code:
  a second token would add nothing a thief of the first lacks, and the
  short-lived part T-060 meant is the 60 s voice lease keyed by the token.
- Kick: confirmed it is only a disconnect; the token keeps working. Kept that
  and documented it (`a_kick_is_only_a_disconnect` pins it). A ban refuses
  every token while it lasts but keeps the rows.

Lifetimes: idle 90 days (unchanged), absolute 180 days from login. Each use
sets expiry to min(now + 90 d, created_at + 180 d), and `Hello`/HTTPS also
check `created_at + 180 d` so rows written before this change are capped too.
180 days is two idle windows: an active user types the password about twice
a year, and a copied token is dead within six months even if the thief keeps
using it. Revocation is now the main remedy, so a shorter cap would mostly
cost friction. No schema change: `created_at` already exists.

Changes:
- Protocol (new tags only): `ClientEnvelope` 42 `Logout`, 43 `ListSessions`,
  44 `RevokeSession{session_id}`; `ServerEnvelope` 32
  `SessionRevoked{reason}`; `Ack` 11 `sessions: SessionList` of
  `SessionInfo{id, device_name, created_at, last_seen_at, expires_at,
  current}`. Close code 6 (`auth::CLOSE_SIGNED_OUT`). Welcome feature
  `sessions`. Welcome 6–7 unused.
- Server: `auth::live::Sessions` maps each connection to its session. Logout
  deletes the row and closes the session's connections; the caller's `Ack`
  rides the kick channel as the last frame (the writer prefers kicks, so a
  normal reply could be overtaken), others get `SessionRevoked`. A sweep
  every 30 s closes connections whose row is gone or expired (covers the CLI,
  which runs in another process), and expired rows are pruned at startup and
  hourly. `set_password` deletes every session in the same transaction.
  Unknown usernames verify against `password::DUMMY_HASH` (same parameters).
- CLI: `ss-server user sessions list --username U` (id, device, created,
  last seen, expires in UTC; never tokens) and `user sessions revoke
  --username U --id ID | --all`. `user set-password` also prints how many
  sessions it ended.
- Client core: `Client::logout` (always stops the client, no reconnect),
  `list_sessions`, `revoke_session`. `SessionRevoked` becomes
  `ClientEvent::Removed { banned: false }`, so reconnect stops.
- Desktop: "Remove server" logs the session out first, through the live
  client or, when disconnected, one connection with the saved token, 5 s
  each, best effort. There is no separate "Sign out" in the app today;
  BACKLOG has it with a devices page.

Tests: `ss-server/tests/sessions.rs` (reset, logout, self revoke incl. another
account's id, operator CLI list/revoke/--all, absolute cap, pruner, kick),
each checking QUIC `Hello`, HTTPS `401` and that open connections get
`SessionRevoked` and stop without reconnecting. Unit tests for the dummy
hash, unknown-username hashing, and the new `db::sessions` deletes. The
desktop onboarding test covers sign-out on remove, connected and offline.
`authorization_audit.rs` classifies the three commands (scoped to the
caller's own sessions). `just check`, desktop `cargo test` and clippy pass.

Files outside Paths: `ss-server/src/connection.rs` (sibling T-091: a
`sessions` field, `attach` before register, one dispatch branch and
`session_answer`, the sweeper spawn in `run`, `session_id` on
`AuthedContext`, the `sessions` feature string; about 50 added lines, nothing
moved); `ss-server/tests/{sessions,auth,authorization_audit}.rs`;
`docs/plan/tasks/BACKLOG.md`. Client `connection.rs` is untouched; logout
lives in `commands.rs`.

Open points:
- A connection authenticated in the instant before a revoke, and attached
  after it, stays open until the next sweep (at most 30 s).
- The desktop does not show the `Removed` reason; a revoked session just
  goes disconnected and the next saved-token connect fails as
  unauthenticated.
- Hands-on: in the app, remove a connected server and check that the old
  token (from `sessions.json` before removal) is refused; run `ss-server
  user sessions revoke` against a running server and see the client drop
  within 30 s without a reconnect loop.
