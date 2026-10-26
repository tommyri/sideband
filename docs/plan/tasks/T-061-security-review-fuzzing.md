# T-061 — Security review, fuzzing, rate limits audit

Status: todo
Phase: 4
Size: M
Depends on: T-021, T-022, T-026, T-027, T-029, T-030, T-031
Paths: `fuzz/`, `docs/security.md`, fixes across `ss-server`

## Goal
A self-hosted server exposed to the internet shouldn't be a liability.

## Scope
- Threat model doc (`docs/security.md`): assets, trust boundaries (internet
  → QUIC/HTTPS, members → server, admins), what TOFU protects and doesn't.
- `cargo-fuzz` targets: control framing + protobuf decode, datagram header,
  markdown/mention parser (shared with client via `ss-core`?), OG/HTML
  parser, multipart upload parser; run in CI nightly for 10 min each.
- Authorization audit: script that enumerates every handler and asserts a
  `perms.require` call exists (clippy-lint-like test over source, or a
  registry pattern that makes it impossible to forget).
- Rate limits review: Hello/auth, messages, typing, uploads, previews,
  webhooks, invite brute-force; per-IP and per-user; `429 RATE_LIMITED` with
  retry-after.
- Input limits: username/channel/role name lengths, Unicode normalization
  (NFC) and confusable check for usernames, max attachments per message, max
  mentions.
- Dependency audit: `cargo deny` advisories in CI; pin `rustls` provider
  (ring or aws-lc-rs) consistently.
- Secrets handling: tokens zeroized (`zeroize`), never logged; argon2
  parameters documented.
- HTTP hardening: strict headers, no directory listing, request body limits,
  timeouts, `Content-Type` enforcement.

## Out of scope
External pentest.

## Acceptance criteria
- All fuzz targets run 1 h locally without crashes/timeouts.
- Authorization audit test passes and fails if a handler is added without
  a permission check (demonstrate in PR).
- `docs/security.md` reviewed by a second person (note who).

## Notes from implementation
