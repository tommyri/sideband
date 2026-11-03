# Security

What a self-hosted Sideband server protects, from whom, and how. Written
for T-061 against the server at the end of phase 2 (T-020 to T-034).

> **Review status:** the acceptance criterion "reviewed by a second person"
> is **pending**. The maintainer has to name the reviewer here:
> `Reviewed by: <name>, <date>`.

## 1. Assets

| Asset | Where it lives | Why it matters |
|---|---|---|
| Password hashes | `users.password_hash` (argon2id PHC strings) | Offline cracking leads to reuse on other services |
| Session tokens | Client only. Server stores SHA-256 hex in `sessions.token_hash` | A token is a full login for 90 days |
| Owner bootstrap code | `server_settings.owner_invite` until the first registration | Grants `Admin` |
| Invite codes | Client only. Server stores SHA-256 hex in `invites.code_hash` | Registration on an invite-only server |
| Webhook tokens | Client only. SHA-256 hex in `webhooks.token_hash` | Posting into a channel as a bot |
| Attachment signing key | `<data>/attachment.key`, mode 0600 | Forges 10-minute download links |
| TLS private key | `<data>/tls/identity.json` (self-signed), operator PEM files, or `tls/acme/` | Impersonating the server |
| Message history, DMs, attachments | `socialspeak.db`, `attachments/` | Privacy of the group |
| Voice | Never stored. Opaque Opus datagrams | Eavesdropping in transit |
| Availability | One process, one SQLite writer | A friend group with no voice chat |

## 2. Trust boundaries

```
 internet ──UDP 443 (QUIC, TLS 1.3)──► ss-server ◄──TCP bind_http (HTTPS)── internet
                                          │
                                          ├── SQLite + data dir  (trusted, operator-owned)
                                          ├──► link-preview fetches to the internet (untrusted responses)
                                          └── 127.0.0.1:9101 /healthz /metrics (internal only)
```

| Boundary | Who is on the other side | What they can do before any check | Main controls |
|---|---|---|---|
| Internet → QUIC | Anyone | Complete a handshake, send one `Hello` | Stateless Retry before any slot, 256 sessions total, 32 per validated address, 10 s handshake and Hello deadlines, Hello and password quotas, 1 MiB frames |
| Internet → HTTPS | Anyone | Complete a TLS handshake, then send requests; `GET /api/v1/server` and webhook posts need no bearer | Same live certificate and pin as QUIC, handshake deadline, per-address quota, header and request deadlines, body caps, strict headers |
| Members → server | Authenticated accounts | Every control command and REST call | `PermissionService::check` per command, role hierarchy, per-account quotas, input limits |
| Moderators and admins → server | Accounts with management bits | Kick, ban, mute, move, roles, channels, webhooks, invites | Bits plus strict hierarchy (`can_act_on`); only `ADMINISTRATOR` grants `ADMINISTRATOR`; audit log |
| Server → previewed sites | Arbitrary web servers | Return any bytes to the preview fetcher | SSRF address filter after DNS, pinned socket address, 3 redirects re-checked, 5 s, 1 MiB, fuzzed parser |
| Operator → server | Whoever has shell on the host | Everything | Out of scope. The data dir is the trust root |

The unauthenticated voice spike (`ss-server` with no subcommand) accepts any
username. It binds `127.0.0.1:4433` by default and must not be exposed.

## 3. TOFU: what it protects and what it does not

Self-signed is the default (decision D10). The client pins the SHA-256 of the
leaf on first connect and refuses a different one until the user re-approves
(`ss_net::pins`). A WebPKI-valid chain is accepted and also recorded.

It protects against:

- an attacker who appears **after** the first connection (café Wi-Fi, a
  hijacked route, a DNS change): the pin no longer matches;
- passive eavesdropping on QUIC at any time (TLS 1.3);
- a later certificate swap on the server going unnoticed (`tls rotate`
  re-prompts every client).

It does not protect against:

- an attacker on the path during the **first** connection: the user pins the
  attacker's key. Mitigation is out of band: compare the fingerprint printed
  by `ss-server run` / `ss-server fingerprint` (also at `GET /api/v1/server`,
  which is itself unauthenticated, so only an out-of-band channel counts);
- users who click through a changed-fingerprint prompt;
- a compromised server host or a stolen `tls/identity.json`: the pin matches;
- an attacker who controls the out-of-band fingerprint channel.

## 4. Authentication and secrets

- **Passwords**: argon2id, version 0x13, memory 19 456 KiB (19 MiB), 2
  iterations, 1 lane, 16-byte random salt, PHC string output
  (`auth::password`). This is the OWASP minimum recommendation. Hashing runs
  on `spawn_blocking`. Passwords are 8 to 128 bytes; a longer login is
  rejected before hashing.
- **Usernames**: 3 to 32 of `a-z 0-9 _ .`. ASCII only, so NFC is the identity
  on every accepted name. A new name is refused when its confusable skeleton
  (`rn`→`m`, `vv`→`w`, `cl`→`d`, `0`→`o`, `1`/`i`→`l`, `_`→`.`) matches an
  existing account (`auth::username_skeleton`).
- **Session tokens**: 32 bytes from the OS RNG, base64url. Only the SHA-256
  hex digest is stored. Expiry slides to 90 days on each use. A presented
  token is looked up on a read connection first so a flood of bogus bearer
  tokens never queues on the SQLite writer.
- **Owner code**: 16 random bytes, stored in `server_settings` until the
  first registration, deleted when any user exists. Printed to stdout only;
  it is not written to the log files.
- **Invite codes**: 16 random bytes (128 bits), SHA-256 stored and returned
  once to the creator. The migration revokes legacy plaintext codes and logs
  how many were invalidated.
- **Webhook tokens**: 16 random bytes, SHA-256 stored, returned once.
- **Attachment links**: HMAC-SHA256 over id and expiry, 10-minute lifetime,
  32-byte key created with mode 0600 before any byte is written.
- **Zeroization** (`zeroize`): issued session tokens, password copies on the
  hashing thread, the credential fields of every `Hello` once checked, and
  the attachment signing key on drop. Tokens also exist in prost messages and
  the client; zeroization reduces lifetime in freed memory, it is not a
  guarantee.
- **Never logged**: `Debug` for `db::User` and `auth::Authenticated` prints
  `<redacted>` for hashes, codes, and tokens
  (tested). No `tracing` call takes a token, password, or code (checked by
  grep for T-061). There is no HTTP request logging, so `?token=` download
  links do not reach logs.

## 5. Authorization

`ss_core::Permissions` and the resolution rules are in
`docs/plan/03-data-model.md`. Every authenticated command is dispatched to a
handler that calls `PermissionService::check` with the bit the data model
assigns. Commands that need no bit are scoped by construction: own voice
flags, own presence, DMs (membership is the grant; only the creator adds).

`crates/ss-server/tests/authorization_audit.rs` enforces this:

- every `ClientEnvelope` variant must have a policy in its table;
- every `Msg::X(..) =>` dispatch arm under `src/` must match a policy for
  that file, and the handler (plus the same-file helpers it calls) must name
  each required `Permissions::BIT`, call a check, and reach the listed guards
  (`can_act_on`, `membership`, author or creator comparisons);
- every `.route(..)` must have a policy and reach `caller(` plus its checks.

Behavior is covered by `tests/permissions.rs` (table-driven) and the feature
suites. T-061 fixed two gaps the audit surfaced: `UpdateProfile` changed the
display name without `CHANGE_NICKNAME`, and `ServerMuteUser` did not apply the
role hierarchy, so a moderator could server-mute the owner.

## 6. Rate limits

All quotas are in-memory GCRA buckets (`governor`) for the process lifetime.
Per-address keys use the IPv4 address or the IPv6 /64. A control-stream
rejection is `Error{code: RATE_LIMITED, retry_after_ms}`; HTTP is `429` with
`Retry-After` in whole seconds, rounded up.

| Surface | Key | Quota | On rejection |
|---|---|---|---|
| Unvalidated QUIC Initial | source address | none counted: answered with a stateless Retry | client retries with the token |
| QUIC sessions, pending included | server | 256 validated | handshake refused |
| QUIC sessions, pending included | validated address | 32 | handshake refused |
| Handshake, control stream, `Hello` | connection | 10 s each | closed |
| `Hello`, any credential | address | burst 30, then 1 per 2 s | `RATE_LIMITED` + retry, closed |
| Password login and register | address | burst 5, then 1 per 12 s | `RATE_LIMITED` + retry, closed |
| Failed password logins | username | 10 failures, then 1 per 6 min; when over, only addresses that failed against it in the last 15 min wait | `RATE_LIMITED` + retry, closed |
| Invite brute force | — | 128-bit codes behind the two rows above | — |
| Control commands except `Ping` | account | burst 100, then 20 per s | `RATE_LIMITED` + retry |
| `SendMessage` | account | burst 5, then 1 per 5 s | `RATE_LIMITED` + retry |
| `Typing` | account and channel | 1 per 5 s | `RATE_LIMITED` + retry |
| Webhook posts | webhook | burst 30, then 1 per 2 s | `429` + `Retry-After` |
| Uploads | account | burst 10, then 1 per 6 s | `429` + `Retry-After` |
| Upload bytes | account | 500 MiB per rolling 24 h | `429` + conservative 24 h `Retry-After` |
| Public HTTP requests | address | burst 200, then 50 per s | `429` + `Retry-After` |
| Public HTTP connections | listener | 1024 open | accept waits |
| Public HTTP connections | address | 64 open | closed on accept |
| Public HTTP connection lifetime | connection | closed after 60 s with no bytes either way, and at 10 min of age (graceful, then hard after 300 s); HTTP/1 keep-alive and HTTP/2 alike | connection closed |
| Link previews | server | queue of 32 (full drops), 2 workers, one fetch per host at a time, 3 URLs per message | dropped silently |
| Outbound control queue | connection | bounded; a slow reader is closed | `ServerNotice`, close |
| Media datagrams | connection | header, track ownership, `SPEAK`, server mute validated; 1.5× the channel profile maximum over 1 s; full send buffers drop | excess dropped and counted; repeated excess gets `RATE_LIMITED` then close; never awaited |

The username quota counts failures only and never holds back an address
that has not failed against that account, so knowing a username is not
enough to lock its owner out: a correct password from a clean address
always gets through (tested). Once an account is over its failure budget, a
password-guessing botnet gets one guess per address per 15 minutes on top
of the per-address gate. The owner is held back only if they mistyped from
the same address during an attack, for at most the remaining lock. Existing
sessions (token `Hello`) are unaffected.

These login buckets deliberately remain process-local. This is a small,
self-hosted server whose restarts are controlled by its operator, not by an
internet client. Persisting failed usernames would turn stale guesses into a
durable denial-of-service state while adding no protection across replicas
(there are none). The independent address quota remains in force, and a clean
address with the correct password is never held back by another address's
failures.

Every QUIC connection whose source address has not been validated gets a
stateless Retry before it is counted against any session slot, so spoofed
source addresses cannot use up a victim's per-address slots or the server's
pending-handshake budget.

## 7. Input limits

| Field | Limit |
|---|---|
| Control frame | 1 MiB protobuf, length-prefixed |
| Username | 3–32 of `a-z 0-9 _ .`, confusable skeleton unique |
| Password | 8–128 bytes |
| Display name | 1–64, trimmed, NFC, no hidden characters (below); changing it needs `CHANGE_NICKNAME` |
| Channel and group-DM name | 1–64, same rules |
| Channel topic | ≤ 1024 |
| Role name | 1–100, same rules |
| Webhook name and `username` override | 1–80, same rules |
| Message body | 1–4000 after trimming; webhook bodies ≤ 2000 |
| Mentions per message | ≤ 50 distinct `<@ULID>` |
| Attachments per message | ≤ 10, each listed once, uploaded by the sender, unused |
| `SendMessage.nonce` | ≤ 128 |
| Presence activity | ≤ 128 |
| Kick/ban reason | ≤ 500 |
| Group DM | caller plus 2–9 others |
| Voice user limit | 0–99 |
| History page | ≤ 100 |
| Upload | ≤ 25 MiB by default (`limits.upload_max_bytes`), one file per request, file name normalized to ≤ 200 |

Lengths count Unicode scalar values after normalization. "Hidden" is decided
by Unicode property, not a list (`ss_server::text`, data from
`icu_properties`): general categories Cc, Cf, Zl, Zp, Co (private use), and Cn
(unassigned), plus every `Default_Ignorable_Code_Point`. That covers bidi
marks, overrides and isolates, tag characters, zero-width space and
non-joiner, the combining grapheme joiner, Hangul fillers, the Mongolian
vowel separator, the soft hyphen, and all variation selectors. The only
exceptions are U+200D (zero-width joiner) and U+FE0E/U+FE0F (text and emoji
presentation), so emoji sequences such as families, keycaps, skin tones,
and the rainbow flag still work. Subdivision flags (tag sequences) do not.
Message bodies are stored as sent; clients render them as text.

Only usernames must be unique, and they are compared by confusable skeleton
(section 4). Display, channel, and role names have no uniqueness rule, so
there is no duplicate check to run under NFKC. A display name can still
imitate another member's name. The protocol always includes the immutable
username in each `User` carried by snapshots and member/message events, and
the voice and text UI tasks require rendering it beside every display name.

## 8. Transport and HTTP hardening

- QUIC: TLS 1.3 only, ALPN `socialspeak/1`, datagrams required, 30 s idle
  timeout. rustls uses the **ring** provider everywhere (QUIC, ACME, preview
  fetches); `deny.toml` bans `aws-lc-rs`, `aws-lc-sys`, OpenSSL, and
  `native-tls`.
- HTTPS accept loop (`http::hardening::serve_tls`): 10 s to complete the TLS
  handshake, then 10 s to send each HTTP/1
  request head, at most 1024 open connections and 64 per address (IPv4, or
  IPv6 /64). A connection that moves no bytes for 60 s, or reaches 10 min of
  age, gets a graceful shutdown (HTTP/1 finishes its request, HTTP/2 sends
  GOAWAY) and is dropped after a grace period.
- Every public response gets `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`,
  `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'; sandbox`,
  and `Cache-Control: no-store` unless the route sets its own (attachments
  use `private, no-cache` with an ETag).
- Request deadline 30 s, 300 s for attachment transfers (`408`).
- Body cap 64 KiB on every route; uploads alone keep their own cap (the
  upload limit plus 256 KiB of multipart framing).
- `Content-Type`: webhook posts must be `application/json` (`415`); uploads
  must be `multipart/form-data`.
- No static file serving, so no directory listing: unknown paths are `404`.
  Attachments are read only by database id through a path that cannot leave
  the data directory.
- Downloads: sniffed MIME only (client type ignored), executables refused
  unless allow-listed, `Content-Disposition: attachment` except images and
  video, and the CSP `sandbox` keeps an opened HTML or SVG file inert.

### 8.1 REST transport

`bind_http` accepts TLS only and uses the same hot-reloaded certificate
resolver as QUIC. Clients verify the same leaf fingerprint for history and
attachment requests and do not follow redirects. Plain HTTP on that port is
closed during the TLS handshake. The internal `127.0.0.1:9101` health and
metrics listener remains plain HTTP and must stay private. ACME HTTP-01 uses a
separate TCP 80 listener for challenges and redirects all other paths to the
configured HTTPS port.

## 9. Link previews (SSRF)

The fetcher resolves the host, drops every non-public address, and pins the
socket to the first permitted one, so DNS rebinding cannot switch targets
between the check and the connect. Each redirect hop is re-checked; userinfo
URLs are refused. Rejected ranges: IPv4 0/8, 10/8, 100.64/10, 127/8,
169.254/16, 172.16/12, 192.0.0/24, 192.0.2/24, 192.88.99/24, 192.168/16,
198.18/15, 198.51.100/24, 203.0.113/24, 224/4, 240/4, broadcast; IPv6 `::`,
`::1`, `::/96` (IPv4-compatible), `::ffff:0:0/96` judged by its IPv4 address,
`64:ff9b::/96` judged by its IPv4 address, `64:ff9b:1::/48`, `100::/64`,
`2001::/32` (Teredo), `2001:db8::/32`, `2002::/16` judged by its embedded
IPv4 address, `fc00::/7`, `fe80::/10`, `fec0::/10`, `ff00::/8`. Image URLs
in a card are stored, not fetched, by the server; clients that load them
reveal their own address to that site.

## 10. Fuzzing

`fuzz/` is a separate cargo-fuzz workspace. Targets:

| Target | Input | Checks |
|---|---|---|
| `control_frames` | control frames, chunked reads, both envelope types | no panic, round-trip equality, oversize prefixes rejected |
| `media_headers` | datagram and video stream headers | byte-exact re-encode |
| `mentions` | message bodies | mention parser and preview URL extractor invariants |
| `preview_html` | fetched HTML plus oEmbed JSON | terminates, clipped fields, http(s)-only URLs |
| `multipart_upload` | upload request bodies | size cap, file-name normalization |

There is no Markdown parser on the server; it stores raw text. `.github/
workflows/fuzz.yml` runs every target for 10 minutes nightly. The first run
of `preview_html` found a hang: a non-name byte inside a tag stalled the
attribute loop forever (fixed in `previews::parse`, regression test added).

## 11. Dependencies

`cargo deny check` runs in CI: RustSec advisories and yanked crates fail,
licenses are a permissive allow-list with two scoped exceptions
(`webpki-roots`: CDLA-Permissive-2.0; `audio_thread_priority`, client only:
MPL-2.0), crates.io is the only source. T-061 dropped the unmaintained
`rustls-pemfile` (RUSTSEC-2025-0134) for `rustls-pki-types` PEM parsing and
updated `h2` past RUSTSEC-2026-0258.

## 12. Known gaps

- Fuzz targets have not yet run the 1 h each that T-061 asks for. Tracked as T-067.
