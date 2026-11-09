# T-093 — Session revocation, logout and expiry

Status: todo
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
