# T-021 — Auth: registration via invite, login, sessions, owner bootstrap

Status: todo
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
