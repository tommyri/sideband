# Security

What a self-hosted Sideband server protects, from whom, and how. Written
for T-061 against the server at the end of phase 2 (T-020 to T-034).

> **Review status:** the acceptance criterion "reviewed by a second person"
> is **pending** (T-067). The maintainer has to name the reviewer here:
> `Reviewed by: <name>, <date>`. The 2026-11-09 code review at `d861a0a` was
> a full read of the code, not that second-person review; what it changed in
> this document, and what is still open, is in section 13.

## 1. Assets

| Asset | Where it lives | Why it matters |
|---|---|---|
| Password hashes | `users.password_hash` (argon2id PHC strings) | Offline cracking leads to reuse on other services |
| Session tokens | Client only. Server stores SHA-256 hex in `sessions.token_hash` | A token is a full login, on QUIC and HTTPS, until it is unused for 90 days or 180 days after login, whichever is first, or until the session is ended (section 4) |
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
| Internet → HTTPS | Anyone | Complete a TLS handshake, then send requests; `GET /api/v1/server` and webhook posts need no bearer | Same server certificate as QUIC (a reverse proxy may use another valid domain certificate for public-trust clients), handshake deadline, per-address quota, header and request deadlines, body caps, strict headers, attachments streamed from and to disk with 16 open transfers per account |
| Members → server | Authenticated accounts | Every control command and REST call | `PermissionService::check` per command, role hierarchy, per-account quotas, input limits |
| Moderators and admins → server | Accounts with management bits | Kick, ban, mute, move, roles, channels, webhooks, invites | Bits plus strict hierarchy (`can_act_on`); only `ADMINISTRATOR` grants `ADMINISTRATOR`; audit log |
| Server → previewed sites | Arbitrary web servers | Return any bytes to the preview fetcher | SSRF address filter after DNS, pinned socket address, proxy variables ignored, 3 redirects re-checked, 5 s, 1 MiB, fuzzed parser |
| Operator → server | Whoever has shell on the host | Everything | Out of scope. The data dir is the trust root |

The unauthenticated voice spike (`ss-server spike`) accepts any username. It
binds `127.0.0.1:4433` by default and must not be exposed. `ss-server` with no
subcommand prints usage and exits; it never starts a listener.

## 3. TOFU: what it protects and what it does not

Self-signed is the default (decision D10). The client pins the SHA-256 of the
leaf on first connect and refuses a different one until the user re-approves
(`ss_net::pins`). New servers with a WebPKI-valid chain use a persisted public-certificate policy,
not a leaf pin. Each QUIC/HTTPS handshake validates the configured hostname,
validity period and chain. Valid renewals are accepted; validation failure never
falls back to TOFU. HTTPS credentials use the policy captured with the authenticated
QUIC session, not a subsequent mutable-store lookup. Redirects remain disabled.

The version-2 trust file migrates all old fingerprints to strict pins, since the
legacy format did not record whether approval was explicit or via a public CA.
Switching to domain trust is an explicit disconnected-client action and validates
both transports before atomically replacing the old policy. Future/invalid trust
file formats fail closed. Self-signed first-use prompts require a currently valid,
self-signed certificate; already approved exact pins retain their existing identity
semantics (including no hostname/expiry revalidation). Removing a saved server
retains its trust decision. Public trust relies on CA/domain authority and permits
normal key rotation; an explicit pin is deliberately stricter.

Explicit pinning protects against:

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
  rejected before hashing. A login for an unknown username is verified
  against a fixed dummy argon2id hash with the same parameters, so it takes
  as long as a wrong password (tested).
- **Usernames**: 3 to 32 of `a-z 0-9 _ .`. ASCII only, so NFC is the identity
  on every accepted name. A new name is refused when its confusable skeleton
  (`rn`→`m`, `vv`→`w`, `cl`→`d`, `0`→`o`, `1`/`i`→`l`, `_`→`.`) matches an
  existing account (`auth::username_skeleton`).
- **Session tokens**: 32 bytes from the OS RNG, base64url. Only the SHA-256
  hex digest is stored. Two lifetimes (T-093): a token unused for 90 days
  expires, and every token expires 180 days after the login that issued it
  however often it is used. Each use moves expiry to the earlier of now +
  90 days and login + 180 days, so a token presented every 89 days still
  dies. `Welcome.resume_token` is this session token, and it is also the
  HTTPS bearer. There is no separate short-lived resume token; the only
  short-lived part of a resume is the 60-second voice-seat lease keyed by
  the token (T-060). A presented token is looked up on a read connection
  first so a flood of bogus bearer tokens never queues on the SQLite writer.
- **Ending sessions** (T-093): a token stops working when its `sessions` row
  is deleted. That happens on `Logout` (this session), `RevokeSession` (one
  of the caller's own sessions), `ss-server user sessions revoke --username U
  --id ID` or `--all` (the operator), and `ss-server user set-password`
  (every session of the account, in the same transaction as the new hash).
  Every `Hello` and every HTTPS request reads the row, so the next request
  fails at once. Live QUIC connections that used the token get
  `SessionRevoked` and close code 6: at once when the server itself ran the
  revoke, and within 30 seconds (the sweep interval) when the CLI changed the
  database from another process. Expired rows are deleted at startup and then
  hourly. A stolen token can therefore be killed without banning the victim
  or editing the database.
- **Kicks and bans**: a kick is only a disconnect. The session stays valid
  and the member can reconnect at once (tested). A ban keeps the rows but
  refuses every token of the account, on `Hello` and HTTPS, while it lasts;
  lifting it makes them work again. After a theft, reset the password or
  revoke the sessions instead.
- **Owner code**: 16 random bytes, stored in `server_settings` until the
  first registration, deleted when any user exists. Printed to stdout only;
  it is not written to the log files.
- **Invite codes**: 16 random bytes (128 bits), SHA-256 stored and returned
  once to the creator. The migration revokes legacy plaintext codes and logs
  how many were invalidated.
- **Webhook tokens**: 16 random bytes, SHA-256 stored, returned once.
- **Attachment links**: HMAC-SHA256 over attachment id, signer (the
  uploader's account id) and expiry; the token carries expiry, signer and MAC
  (layout in `02-protocol.md` §6, T-092). 10-minute lifetime, 32-byte key
  created with mode 0600 before any byte is written. Signed downloads use
  their own pool of 16 open transfers per signer, so a leaked link cannot
  use up the uploader's own transfer slots.
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

- every outbound path in its `OUTBOUND` list (the actor's reply, fan-out,
  the voice-state gate, seat refresh and resume, `MoveUser`'s view checks,
  the CLI ban) must keep reaching its visibility or revocation call, and the
  audit-log query must keep hiding `dm.*` rows.

Behavior is covered by `tests/permissions.rs` (table-driven), `tests/revocation.rs`,
and the feature suites. T-061 fixed two gaps the audit surfaced: `UpdateProfile`
changed the display name without `CHANGE_NICKNAME`, and `ServerMuteUser` did not
apply the role hierarchy, so a moderator could server-mute the owner.

T-090 closed the gaps a later review found, each with a test that failed first:

- *Granting.* Role creates, role edits, and role assignments could add any bit
  except `ADMINISTRATOR`, so a `MANAGE_ROLES` holder could put `MANAGE_SERVER`
  or `BAN_MEMBERS` on `@everyone`. Override `allow` masks were unlimited. Now
  only bits the actor holds can be added: server-wide for roles, in that
  channel for overrides.
- *Hidden channels.* Overrides and channel update, move, and delete needed only
  server-wide `MANAGE_CHANNELS`, so a manager could allow `VIEW_CHANNEL` on a
  channel hidden from them and read it. Every channel command now needs
  `VIEW_CHANNEL` on each channel it touches; a hidden one is not found.
- *Voice seats.* The post-command refresh only updated the speak bit, and a
  resume restored a seat unchecked. A member who loses `VIEW_CHANNEL` or
  `CONNECT` is now removed and stops receiving datagrams; resume rechecks.
- *Administrators.* `03-data-model.md` promised an owner who always wins. There
  is none: administrators are peers, a non-administrator can no longer act on
  an administrator even from a higher role position, and the seeded Admin role
  cannot be deleted or stripped, so `role grant-admin` keeps working.
- *Leaks.* The actor's own reply skipped the fan-out filter, so muting someone
  in a hidden voice channel returned that channel's id; replies now pass the
  same filter. A leave (`VoiceStateUpdated` without a channel) went to every
  session; the registry now sends it only to connections that were told about
  the arrival. `MoveUser` needs the actor to see both channels. The audit-log
  API no longer returns `dm.*` rows.
- *Invites.* A ban left the member's invites usable, and `CREATE_INVITES` is
  in the default role. A ban, from the command or the CLI, now revokes them.

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
| `CreateDm` that would open a new conversation | account | burst 10, then 1 per 6 s; reopening an existing 1:1 DM is free | `RATE_LIMITED` + retry |
| `Typing` | account and channel | 1 per 5 s | `RATE_LIMITED` + retry |
| Webhook posts | webhook | burst 30, then 1 per 2 s | `429` + `Retry-After` |
| Uploads | account | burst 10, then 1 per 6 s | `429` + `Retry-After` |
| Upload bytes | account | 500 MiB per rolling 24 h | `429` + conservative 24 h `Retry-After` |
| Public HTTP requests | address | burst 200, then 50 per s | `429` + `Retry-After` |
| Public HTTP connections | listener | 1024 open | accept waits |
| Public HTTP connections | address | 64 open | closed on accept |
| Public HTTP connection lifetime | connection | closed after 60 s with no bytes either way, and at 10 min of age (graceful, then hard after 300 s); HTTP/1 keep-alive and HTTP/2 alike | connection closed |
| Link previews | server | queue of 32 (full drops), 2 workers, one fetch per host at a time, 3 URLs per message | dropped silently |
| Outbound control queues | connection | 32 frames for replies and bus events (the session waits for room); frames other tasks address to it (moves, stream hints, keyframe requests) held past 32 for a 10 s grace, at most 1024, with one pending `SubscriberHint` and one `RequestKeyframe` per track; event bus 1024 behind; one write stalled 10 s | `ServerNotice`, close with code 2; never a dropped frame on an open connection |
| Guild channel-view diff | session | at most once per 100 ms for a burst of role, override or guild channel changes; DM events never trigger it | — |
| Session `last_seen_at` write | session token | at most once per 60 s; a token Hello or HTTP request within that reads only | — |
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

T-091 bounded what one member can make others pay for. Before it, group
DMs were unlimited and every one went into each member's `Welcome`, so about
4,000 with long names pushed a target's `Welcome` past 1 MiB and the server
refused the login ("welcome response is too large"), owner included, until
someone edited the database. Every DM event made every session re-read every
channel, and a burst of events larger than the 32-frame queue was dropped
silently while the connection stayed open (measured: 33 of 90 presence
updates arrived after one command waited on the writer). One command that
published more than the bus held (deleting a category of hundreds of
channels) closed every other session. Each has a test in
`tests/dm_fanout.rs` that failed first.

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
| Roles | ≤ 250 per server, counting `@everyone` and Admin (`limits::MAX_ROLES`); `CreateRole` at the cap fails with `FORBIDDEN` |
| Webhook name and `username` override | 1–80, same rules |
| Message body | 1–4000 after trimming; webhook bodies ≤ 2000 |
| Mentions per message | ≤ 50 distinct `<@ULID>` |
| Attachments per message | ≤ 10, each listed once, uploaded by the sender, unused |
| `SendMessage.nonce` | ≤ 128 |
| Presence activity | ≤ 128 |
| Kick/ban reason | ≤ 500 |
| Group DM | caller plus 2–9 others |
| Group DMs started | ≤ 100 live per account, counted even after the creator leaves |
| Group DMs joined | ≤ 200 live per account; leaving frees a slot. Creating or adding fails with `FORBIDDEN` for anyone at the cap |
| DMs in `Welcome` | the 100 most recently active, halved further if the frame would pass 1 MiB; `ListDms` pages ≤ 100 load the rest |
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

**`Welcome` size.** Apart from DMs, a `Welcome` is sent in full: every
guild channel the member can see, every role, every member, every voice seat.
`tests/welcome_bound.rs` measures it with the snapshot builder and the
protobuf encoder. "Longest" puts every string at its longest allowed length
in four-byte characters, with an avatar on every member; "typical" uses
ASCII names of about 11 characters, no topic, avatar or activity.

| Item | Longest | Typical |
|---|---|---|
| Member (with the `@everyone` id) | 359 B | 70 B |
| Activity of an online member (at most 256, one per connection) | +516 B | 0 |
| Role | 435 B | 43 B |
| Role a member holds, besides `@everyone` | 18 B | 18 B |
| Guild channel (voice, every setting, full topic) | 4,440 B | 85 B |
| Read state, one per channel at most | 44 B | 44 B |
| Voice seat (state and track), one per connection at most | 138 B | 138 B |
| User, server info, token | about 2 KB | about 2 KB |

The bound that holds, with M members, R roles, G role grants and C channels:
with longest strings and all 256 connections online with a full activity and
a voice seat, a `Welcome` fits one 1 MiB frame while
359·M + 435·R + 18·G + 4,484·C ≤ 870,000 bytes (1 MiB less 132 KB of
activities, 35 KB of seats, and the fixed part). With typical strings the
same holds for 70·M + 43·R + 18·G + 129·C ≤ 1,000,000. Measured examples,
each at the role cap (`@everyone`, Admin and 248 more): 50 channels and
1,000 members holding five roles each, all longest, is 913 KB; 200 channels
and 8,000 members holding two roles each, all typical, is 883 KB. The role cap holds roles to 109 KB at
most. Members, channels and grants are not capped: past the bound, every
`Hello` is refused with "welcome response is too large" (and logged) until
the guild shrinks. Paging members would lift that; it is a protocol change
and is not made. DMs never cause the refusal: they are halved until the
frame fits.

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
configured HTTPS port. The TLS-ALPN-01 listener has the same per-address cap
and 10 s handshake deadline as the HTTPS listener and closes each connection
after its handshake. `tls/acme/`, which holds the issued key, is created `0700`.

## 9. Link previews (SSRF)

The fetcher resolves the host, drops every non-public address, and pins the
socket to the first permitted one, so DNS rebinding cannot switch targets
between the check and the connect. `HTTP_PROXY`-style variables are ignored:
a proxy would resolve the name itself and bypass the pin. Each redirect hop is re-checked; userinfo
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
| `control_frames` | one control frame, both envelope types | protobuf decode: no panic, round-trip equality |
| `control_stream` | a control stream in arbitrary chunks, both envelope types | `ss_net::control::FrameDecoder`, the parser both peers run: same messages and outcome as a whole-buffer reference parse, never reads past a frame, oversize prefixes rejected, errors sticky, round-trip equality |
| `video_group` | a video group stream in arbitrary chunks | `ss_net::media::stream::GroupParser` (behind `read_header` and `GroupReader`): same items as one whole-stream feed, items re-encode to exactly the bytes consumed, frame limits hold, errors sticky |
| `media_headers` | datagram and video stream headers | byte-exact re-encode |
| `mentions` | message bodies | mention parser and preview URL extractor invariants |
| `preview_html` | fetched HTML plus oEmbed JSON | terminates, clipped fields, http(s)-only URLs |
| `multipart_upload` | upload request bodies | size cap, file-name normalization |

Until T-101, `control_frames` read its chunks through `ss_proto::read_frame`,
which only tests call; the production readers had no target. Both stream
parsers are now sans-IO state machines that the QUIC readers drive, so the
targets exercise the code that runs.

There is no Markdown parser on the server; it stores raw text. `.github/
workflows/fuzz.yml` runs every target for 10 minutes on manual dispatch. The
first run of `preview_html` found a hang: a non-name byte inside a tag stalled
the attribute loop forever (fixed in `previews::parse`, regression test added).

## 11. Dependencies

`cargo deny check` runs in CI over the graph with every feature enabled:
RustSec advisories and yanked crates fail, licenses are a permissive
allow-list with per-crate exceptions recorded in `deny.toml`
(CDLA-Permissive-2.0 for the Mozilla root stores, MPL-2.0 for
`audio_thread_priority` and Tauri's CSS and directory helpers), crates.io is
the only source. T-061 dropped the unmaintained `rustls-pemfile`
(RUSTSEC-2025-0134) for `rustls-pki-types` PEM parsing and updated `h2` past
RUSTSEC-2026-0258.

Ignored advisories are listed in `deny.toml`, each with its reason. All but
one are maintenance notices. The exception is RUSTSEC-2026-0459 (CVE-2026-119375),
ignored on 2026-11-11 with a review date of 2027-02-11 (T-110):

- SQLite before 3.54.0 can return uninitialized page-cache memory from
  ordinary queries when it opens a database together with a `-wal` file
  whose page size differs. The files must be crafted, and the process must
  open them.
- `libsqlite3-sys` 0.37.0 bundles SQLite 3.51.3. The fix is in 0.39
  (SQLite 3.54), but `sqlx-sqlite` 0.9.0, the newest sqlx, requires
  `<0.38`.
- Only `ss-server` links SQLite; the desktop app and `ss-cli` do not (they
  reach it only as a test dependency). The server opens only
  `socialspeak.db` in its own data directory. A crafted `-wal` there needs
  write access to that directory, which already allows replacing the
  database. Backups from `ss-server backup` contain no WAL, and the restore
  commands in `deploy/README.md` delete the WAL files first. A tar from an
  untrusted source can still carry a `-wal`; restore only your own
  backups.
- Upgrade sqlx and remove the ignore when a release allows
  `libsqlite3-sys` 0.39. `cargo deny` warns about an ignore that no longer
  matches.

Build tooling is pinned too. Actions are pinned by commit, and the Docker
base images and the release BuildKit image (`.github/buildkit/Dockerfile`)
by digest; Dependabot updates all of these. It does not update the Rust
version (`rust-toolchain.toml` and the workflow `toolchain` inputs), the
nightly used for fuzzing, the `cargo-deny` and `cargo-fuzz` versions that
`taiki-e/install-action` installs (the newest, unpinned), the runner images, or
the NSIS and WiX tools that Tauri downloads while bundling.

Release signing keys are kept out of every job that installs or builds
dependencies. The platform build jobs are in no environment and get no secret.
Windows code signing has its own job since T-111 (see `docs/release.md`,
"Windows"): the build runs `tauri build --no-bundle` without the certificate,
and `windows-signing`, on a fresh runner, checks the uploaded executable before
any secret, imports the PFX, and runs `tauri bundle`, which signs the
executable, the NSIS plugins, the uninstaller and both installers. That job runs
no package manager, `cargo build` or dependency script. The only third-party
code it runs after the import is the Tauri CLI, fetched as the two tarballs
`apps/desktop/pnpm-lock.yaml` pins and checked against their sha512 integrity,
`cargo metadata --no-deps`, signtool, and the NSIS and WiX tools Tauri downloads
and checks by hash. The key is imported non-exportable, which code running as
the same administrator user could bypass, so the protection is that nothing
from the build runs in that job. As for macOS and the updater key, the job signs
whatever the build produced: a compromised npm or crates.io dependency can still
ship code inside a signed release, but it cannot use or take the key.

## 12. Known gaps

- Fuzz targets have not yet run the 1 h each that T-061 asks for, and
  `fuzz.yml` has never been dispatched. Tracked as T-067.
- Files saved from chat carry no download mark on Linux, which has no
  equivalent. macOS and Windows mark them (T-104).
- The `windows-signing` job (section 11, T-111) has not run on GitHub yet.
  Until a dispatched test build passes `signtool verify /pa` on the
  executable, the uninstaller and both installers, Windows signing is
  unverified.
- RUSTSEC-2026-0459 (bundled SQLite before 3.54) is ignored in `deny.toml`
  until `sqlx` allows `libsqlite3-sys` 0.39; review by 2027-02-11 (section
  11, T-110).
- Members, channels and role grants are not capped, so past the bound in
  section 7 every `Hello` is refused. About 230 channels with full topics
  reach it alone, so a member with `MANAGE_CHANNELS` can lock everyone out.
  T-112.

## 13. Review of 2026-11-09

A full read of the code at `d861a0a` found defects and plan drift, recorded
as T-081 to T-101 in [`tasks/INDEX.md`](plan/tasks/INDEX.md). The findings
that changed a guarantee this document states:

| Task | What was not true | Now | Status |
|---|---|---|---|
| T-090 | Role and override edits could grant bits the actor lacks; channel commands worked on channels hidden from the actor; seats survived losing `VIEW_CHANNEL` or `CONNECT`; the documented owner did not exist | Fixed, section 5 and `03-data-model.md` | done |
| T-091 | One member could push another's `Welcome` past 1 MiB with group DMs, and event bursts were dropped on open connections | Fixed, sections 6 and 7 | merged; VPS load check open |
| T-092 | Transfers were buffered in memory and uploads read before the permission check; a deleted message's files stayed downloadable; the TLS-ALPN listener had no cap or deadline; `ss-server` with no subcommand started the unauthenticated spike on `0.0.0.0:443` | Fixed, sections 2, 4 and 8, `02-protocol.md` §6 | done |
| T-093 | A stolen token could not be revoked; "resume token" lifetimes were misdescribed | Fixed, section 4 | merged; in-app checks open |
| T-094 | A message deleted while its link preview was fetched was republished in full; permission reloads could race and leave a demoted user's old rights | Fixed (`01-architecture.md`, `02-protocol.md`) | done |
| T-095 | The webview could load the store plugin and read `sessions.json` (every bearer token); the shell skipped the workspace lints | Fixed, `04-conventions.md` Frontend | merged; installed-build checks open |
| T-096 | Evidence exports include server addresses | Open | in progress |
| T-097 | Any branch could publish a signed update; signing secrets were in the whole build's environment; server images were not tied to a tested commit | Fixed in the workflows (`docs/release.md`) except Windows signing (T-110) | merged; needs GitHub environments and a first real run |
| T-098 | `check-deps.sh` passed when `cargo tree` failed; the public-repo check missed `*pins.json`; the sample Compose ran without dropped capabilities or limits | Fixed in `deploy/` | merged; VPS backup/restore open |
| T-101 | The fuzz targets did not exercise the parsers that run; the router rejected messages the protocol allows | Fixed, section 10 | done |
| T-104 | Saved files carry no download mark | `com.apple.quarantine` and `Zone.Identifier` on every saved file; Linux noted in section 12 | merged; Gatekeeper, SmartScreen and Windows CI checks open |
| T-105 | Shell permission hints are derived in several places | Open, section 12 | todo |

Hands-on checks for these tasks are in
[`tasks/ACCEPTANCE.md`](plan/tasks/ACCEPTANCE.md).
