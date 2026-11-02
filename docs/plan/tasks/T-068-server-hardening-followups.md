# T-068 — Server hardening follow-ups from the T-061 review

Status: done
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
- Authenticated voice datagrams use a per-connection one-second budget at
  1.5× the active channel profile's maximum bitrate. The video transport is
  stream-group based rather than datagram based; its existing group byte,
  concurrency, lifetime, and queue limits remain the applicable guardrails.
- Regular invite codes are SHA-256 hashed by the repository and returned only
  in the create acknowledgement. Migration `0004_hash_invites.sql` revokes
  legacy plaintext codes and emits a one-time startup warning with the count.
- The rolling upload-byte quota returns a conservative 24-hour `Retry-After`.
  Failed-login buckets intentionally stay process-local for this single-node,
  operator-restarted threat model. UI tasks T-044 and T-045 now require the
  immutable username beside mutable display names.
