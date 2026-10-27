# T-021 — Auth: registration via invite, login, sessions, owner bootstrap

Status: done
Phase: 2
Size: M
Depends on: T-020, T-004
Paths: `crates/ss-server/src/auth/`, `crates/ss-server/src/connection.rs`

## Goal
Users can register with an invite code, log in, stay logged in via a session
token, and the first user becomes owner.

## Scope
- Password hashing with `argon2` (argon2id, OWASP params) in `spawn_blocking`.
- `Hello` handling: `Register{invite_code,…}` → validate invite (uses,
  expiry, not revoked), username rules (3–32 chars, `[a-z0-9_.]`, case-
  insensitive uniqueness), create user, consume invite, create session;
  `Login{username,password}` → verify, create session; `session_token` →
  look up hashed token, touch `last_seen_at`; `resume_token` → see T-060
  (accept and treat as session token for now).
- Owner bootstrap: on first start with zero users, the server prints a
  one-time **owner invite code** to stdout/log; the user registering with it
  gets the `Admin` role. Also `ss-server invite create --owner` CLI.
- Ban check at `Hello`: active ban → `Error BANNED` with expiry, close.
- Session tokens: 32 random bytes, base64url; stored as SHA-256; `expires_at`
  = 90 days sliding.
- Rate limiting: per-IP login attempts (5/min) with `governor`.
- `Welcome` includes the user and a snapshot (snapshot assembly is T-023/T-024;
  here it may be empty).
- Connection task skeleton: owns control stream, dispatches `ClientEnvelope`
  to handlers with an `AuthedContext { user_id, conn_id, perms_cache }`.

## Out of scope
Roles UI, password reset (owner can reset via CLI: `ss-server user set-password`).

## Acceptance criteria
- Integration tests: register with valid/invalid/expired/used-up invite;
  login right/wrong password; token reuse; banned user rejected; rate limit
  triggers.
- Argon2 verification never runs on the tokio runtime threads (test with a
  runtime that has 1 worker and a concurrent ping).

## Notes from implementation

- Argon2id uses 19 MiB, t=2, p=1, always via `spawn_blocking`. The test runs
  on a current-thread runtime, records the blocking thread id, and completes
  a ping on the worker before the hash is released. Passwords must be 8–128
  bytes. Uppercase usernames are rejected rather than folded; login still
  matches the `NOCASE` column.
- The owner code is the `server_settings` key `owner_invite`, not an
  `invites` row. `created_by` is `NOT NULL`, so an invite cannot exist before
  the first user, and `0001_init.sql` was left unchanged. The code is 16
  random bytes, base64url. `ss-server invite create --owner` rotates it until
  someone registers, then fails. Consuming it grants the seeded `Admin` role.
- Session tokens are 32 random bytes, base64url without padding, stored as
  hex SHA-256. Expiry is 90 days and slides when the token is presented.
  Login and register mint a new session. `resume_token` is accepted as a
  session token (T-060). Welcome echoes the token to present next and carries
  an empty snapshot. `perms_cache` is the OR of the user's roles and
  `@everyone`; channel overrides are T-022.
- Login and register are limited to 5/minute per peer IP. Session tokens are
  not. An active ban returns `BANNED` with the expiry millis in the message
  (the Error proto has no expiry field) and the connection is closed after
  the frame is given time to arrive.
- The process default listener is still the T-016 spike, so existing voice
  clients keep working. Authenticated Hello is `connection::Server`. A spike
  start with zero users still prints the owner invite. Logs go to stderr so
  that line is the command's stdout. `ss-client-core` cannot drive Hello yet;
  the integration tests use `ss-net`.
