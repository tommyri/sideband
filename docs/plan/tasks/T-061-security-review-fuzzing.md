# T-061 — Security review, fuzzing, rate limits audit

Status: done
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

Merged with the acceptance items below still open; they are tracked as
T-067 (fuzz hours, review, licence) and T-066 (TLS for REST). The known
gaps in `docs/security.md` section 12 are T-068.

**Still open (maintainer):**

- *1 h fuzz runs.* Each target ran 5 minutes, not 1 hour (table below).
  Run each for an hour, for example
  `cargo +nightly fuzz run <target> -- -max_total_time=3600 -timeout=10 -rss_limit_mb=2048 -max_len=8192`.
- *Second-person review* of `docs/security.md`: nobody has reviewed it yet.
  Add the reviewer's name at the top of that file.
- *License policy:* `deny.toml` now has a scoped MPL-2.0 exception for
  `audio_thread_priority` (pulled by cpal, client only). Without it,
  `cargo deny check` already failed on `main`. Confirm or replace it.
- *Plaintext REST listener* (High, not fixed). `bind_http` serves plain HTTP,
  so bearer tokens and attachments cross the network unencrypted. Fixing it
  touches ACME HTTP-01 and the smoke test, so it is in `BACKLOG.md` and
  `docs/security.md` section 8.1 rather than in this change.

**Fuzzing.** `cargo-fuzz` 0.13.2, nightly `aarch64-apple-darwin`, ASan.
Flags were `-timeout=10 -rss_limit_mb=2048 -max_len=8192`. Results:

| Target | Duration | Executions | Result |
|---|---|---|---|
| `control_frames` (new) | 301 s | 13.2 M | clean |
| `media_headers` (existing) | 301 s | 3.1 M | clean |
| `mentions` (new; mention parser and preview URL extractor) | 301 s | 0.90 M | clean |
| `preview_html` (new) | 301 s, after the fix | 0.95 M | clean |
| `multipart_upload` (new) | 301 s | 0.89 M | clean |

A second round on the final code ran each target for 120 s: `control_frames`
5.2 M, `media_headers` 1.4 M, `preview_html` 0.38 M, `multipart_upload` 0.39 M,
and `mentions` 0.37 M executions, all clean. In that round `mentions` first
failed after 52 s on its own wrong invariant: it expected the canonical
`<@ULID>` text in the body. ULIDs parse case-insensitively and accept Crockford
aliases, so the body can spell the id differently. The target now compares
parsed ids, and the rerun was clean. Server behavior is fine; a client that
renders mentions must parse them the same lenient way.

Before the fix, `preview_html` found a hang in under a minute. The minimized
input is `*<q\xfd*9`: a byte that cannot start an attribute name (`*`, `<`,
a quote, non-ASCII) inside a tag stalled `previews::parse::element` forever.
Any previewed page could pin a preview worker and its runtime thread at
100 % CPU. Fixed, with a regression test. The server has no Markdown parser
(it stores raw text), so the "markdown/mention" target covers the mention
parser and the preview URL extractor. Server parsers are reached through
`ss_server::fuzzing` (`#[doc(hidden)]`). `.github/workflows/fuzz.yml` runs
all five targets for 10 minutes each night and caches the corpora.

**Authorization audit.** `tests/authorization_audit.rs` reads the source
(strings and comments stripped). It parses the `ClientEnvelope` oneof and
fails if a command has no policy. It finds every `Msg::X(..) =>` arm and
`let Msg::X(..) = msg else` dispatch. It finds every `.route(..)`. It then
checks that each handler, together with the same-file helpers it calls,
names the required `Permissions::BIT`, calls a check, and reaches its guards
(`can_act_on`, `membership(`, author or creator comparisons). Demonstration:
I temporarily added
`Msg::SubscribeTrack(command) => Some(self.force_subscribe(actor, command).await)`
to `handlers/moderation.rs`, with a body that only wrote an audit row, and
deleted the `KICK_MEMBERS` check from `kick`. The test failed with:

```
handlers/moderation.rs: `Msg::KickUser` -> `kick` never names Permissions::KICK_MEMBERS
handlers/moderation.rs: `Msg::KickUser` -> `kick` never calls a permission check
handlers/moderation.rs: `Msg::SubscribeTrack` is dispatched to `force_subscribe` but the audit does not list this file for it; classify it in COMMANDS
```

The file was then restored. A new proto command fails
`every_client_command_is_classified` in the same way.

**Findings fixed** (each has a test in `tests/hardening.rs`, `tests/auth.rs`,
or a unit test):

- Preview parser infinite loop (above).
- `ServerMuteUser` skipped the role hierarchy, so a `MUTE_MEMBERS` moderator
  could server-mute the owner. It now uses `can_act_on`, as kick, ban, and
  move do (`voice/session.rs`).
- `UpdateProfile` changed the display name without `CHANGE_NICKNAME`
  (`handlers/profile.rs`).
- No per-account control-command quota. Any member could flood `EditMessage`,
  `MarkRead`, `CreateInvite`, and so on, each of which takes the single SQLite
  writer. Now burst 100, then 20/s, with `Ping` exempt (`connection.rs`,
  `limits.rs`).
- No per-address session cap, so one host could hold all 256 QUIC slots. Now
  32 per address, pending handshakes included. Follow-up from the automated
  review: slots were charged to the unvalidated source address, so spoofed
  Initials could fill a victim's slots. The accept loop now answers every
  unvalidated Initial with a stateless Retry (`ss_net::server::Incoming::
  retry_or_refuse`, over quinn 0.11.12 `Incoming::retry`), and counts only
  validated addresses. Spoofing cannot be simulated in-process. Verified by
  the quinn API contract (`remote_address_validated()` is true only after a
  Retry token round trip) and by every QUIC integration test passing with
  each connection now going through the Retry path.
- No Hello quota for token resumes. Any bogus bearer or resume token also
  queued on the SQLite writer. Now there is a per-address Hello quota, and
  token lookups use a read connection first (`auth/mod.rs`).
- Login was rate-limited per address only. Now there is also a per-username
  budget, and logins over 128 bytes skip argon2. Follow-up from the automated
  review: the first version charged the username bucket before verifying,
  so anyone could lock an account out. Now only failed logins (wrong
  password or unknown name) are charged: 10, then 1 per 6 min. When an
  account is over budget, only addresses that failed against it in the last
  15 min are held back (`auth/limiter.rs`). Registration no longer charges a
  username. Deviation from the suggested fix: a tainted address is held back
  before verification rather than verified anyway, because verifying would
  let each attacker address keep guessing at the per-address rate. Test
  `hammering_a_username_cannot_lock_its_owner_out`: 12 failures from 12
  addresses lock the name, the failed addresses wait, a fresh attacker
  address gets one guess, and the owner's correct password from a clean
  address succeeds.
- Per-address keys now use the IPv6 /64.
- `RATE_LIMITED` carried no wait. **Protocol change:** `Error.retry_after_ms`
  (field 6, additive) is set on every `RATE_LIMITED`, and
  `ServerEnvelope::rate_limited` builds it. HTTP 429s carry `Retry-After`.
  `02-protocol.md` is updated.
- Public HTTP had no header-read timeout, connection cap, request deadline,
  per-address quota, default body cap, or security headers. `axum::serve`
  sets none of these. The new `http/hardening.rs` adds them all. Webhooks now
  require `application/json` and accept at most 64 KiB. Attachments get
  `CSP: sandbox` and `Cache-Control: private, no-cache`. Uploads are limited
  per account (burst 10, then 1 per 6 s).
- SSRF guard gaps: 0/8, 192.0.0/24, 192.88.99/24, 198.18/15, IPv4-compatible
  `::/96`, NAT64 `64:ff9b::/96` and `/48`, 6to4 `2002::/16` (judged by the
  embedded IPv4), Teredo, `2001:db8::/32`, `100::/64`, and `fec0::/10` were
  allowed (`previews/guard.rs`).
- Input limits. A message may carry at most 10 attachments, each listed
  once. Topics are capped at 1024. Display, channel, group-DM, role, and
  webhook names are trimmed, NFC-normalized, and refused when they contain
  control or invisible formatting characters (`text.rs`). Usernames are
  ASCII, so NFC is the identity on them; registration refuses a name whose
  confusable skeleton matches an existing account.
- Secrets. The owner invite code was written to the log files by
  `tracing::info!` in `main.rs`; it now goes to stdout only. `Debug` for
  `db::User`, `db::Invite`, and `auth::Authenticated` redacts the hash, code,
  and token. `zeroize` (new workspace dependency) wipes issued tokens,
  password copies, checked `Hello` credentials, and the attachment key. The
  key file is now created 0600 instead of being chmodded after the write.
- Dependencies. `cargo deny check` failed on `main`: RUSTSEC-2026-0258 (`h2`,
  updated to 0.4.20), RUSTSEC-2025-0134 (`rustls-pemfile` unmaintained,
  replaced by `rustls::pki_types` PEM parsing in `tls.rs`), and the MPL
  license above. `deny.toml` bans `aws-lc-rs`, `aws-lc-sys`, OpenSSL, and
  `native-tls`, which keeps rustls on the ring provider. Every provider is
  already chosen explicitly as `rustls::crypto::ring` in `ss-net`, and no
  aws-lc crate is in the graph. `hyper`, `hyper-util`, `tower`, and
  `icu_normalizer` became direct dependencies; all were already in the lock
  graph, so the only lockfile changes are the `h2` bump and the removed
  `rustls-pemfile`.

**Second automated review (two more medium findings, fixed):**

- *HTTP connection exhaustion.* The 1024-connection cap was global, so one
  address could hold every slot with idle keep-alive or HTTP/2 connections,
  and neither protocol had an idle or age limit. `http::hardening::serve` now
  allows 64 connections per address (IPv4, or IPv6 /64), keyed the same way
  as the QUIC `PeerSessions`. It closes any connection that moves no bytes
  for 60 s, and any connection 10 minutes old. Both closes start with a
  graceful shutdown and become a hard close after a grace period. An IO
  wrapper records the last byte read or written. The server's own HTTP/2
  keepalive pings were dropped, because they counted as activity; the idle
  close now finds dead peers instead. Tests:
  `one_address_cannot_hold_every_http_connection`,
  `idle_keep_alive_connections_are_closed`, and
  `busy_connections_are_closed_at_their_maximum_age`. With the per-address
  cap and the idle check temporarily disabled, the first two failed.
- *Hidden-character denylist.* `text::hidden` was a hand-written list. It is
  now decided by Unicode property through `icu_properties` (new
  `[workspace.dependencies]` entry, already in the graph through IDNA, so
  the lockfile gains no package and `cargo deny` passes). It rejects general
  categories Cc, Cf, Zl, Zp, Co, and Cn, plus every
  `Default_Ignorable_Code_Point`. The only exceptions are U+200D and
  U+FE0E/U+FE0F. I added private use and unassigned on top of the
  suggestion: a private-use glyph is whatever a font draws, and an
  unassigned code point may become a format character later. The tests add
  U+180E, U+034F, the U+2066/U+2068 isolates, tags U+E0001 and
  U+E0041..U+E007F, U+FE00, U+E0100, U+2028, U+2029, U+00AD, U+E000,
  U+0378, U+FFF9, and U+1D173. These emoji must still be accepted: a family
  ZWJ sequence, ❤️ and ❤︎, a keycap, a skin tone, the rainbow flag, and a
  regional-indicator flag. Subdivision flags, which use tag characters, are
  now refused. Duplicate check: only usernames are unique, and they already
  compare by an ASCII confusable skeleton. Display, channel, and role names
  have no uniqueness rule, so I added no NFKC comparison. That is recorded
  as a known gap.

**Not changed, documented** in `docs/security.md` section 12: there is no
per-sender datagram bitrate cap, invite codes are stored in plaintext,
quotas reset on restart, and the daily upload quota has no `Retry-After`.
