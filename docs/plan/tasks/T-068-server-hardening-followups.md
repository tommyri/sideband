# T-068 — Server hardening follow-ups from the T-061 review

Status: todo
Phase: 4
Size: M
Depends on: T-061
Paths: `crates/ss-server/src/media/`, `crates/ss-server/src/auth/`, `crates/ss-server/src/db/`, `crates/ss-server/src/http/`, `docs/security.md`, `docs/plan/03-data-model.md`

## Goal
Close the known gaps `docs/security.md` section 12 lists after T-061, other
than TLS for REST (T-066).

## Scope
- Per-sender bitrate cap on voice and video datagrams before fan-out: allow
  the channel profile's maximum plus headroom (for example 1.5× the top of
  the profile's bitrate range, measured over one second); drop and count the
  excess, never await. Repeated violations disconnect with a `RATE_LIMITED`
  notice. Counter on `/metrics`.
- Store invite codes hashed, shown once at creation like webhook tokens.
  Append-only migration; `ss-server invite create` prints the code once.
  `docs/plan/03-data-model.md` updated.
- `Retry-After` on the daily upload byte quota's `429`.
- Persistent lockout decision: either persist the per-username failed-login
  budget across restarts (SQLite table with expiry) or document in
  `docs/security.md` why the in-memory quota is enough for this threat model.
- Display-name impersonation: pick one and implement it. Either the server
  refuses a display name whose confusable skeleton matches another member's
  username or display name in the same server, or the protocol guarantees
  clients always have the username available next to the display name and
  the UI tasks (T-044, T-045) are told to render it. Record the choice in
  `docs/security.md` section 7.
- Shared `nnnoiseless` comment in the root `Cargo.toml` should say it is
  used for denoising and speech probability (T-052 and T-053 both use it).

## Out of scope
TLS for the REST listener (T-066); external penetration test.

## Acceptance criteria
- Integration test: a member sending at twice the profile maximum is
  throttled and other members' audio is unaffected.
- Invite codes in the database are hashes; an existing plaintext row is
  migrated or invalidated with a logged notice.
- `docs/security.md` section 12 lists only accepted, documented gaps.

## Notes from implementation
