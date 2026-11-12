# T-080 — Bot API: bot accounts, tokens, REST + event stream, docs

Status: done
Phase: 6
Size: M
Depends on: T-021, T-022, T-026, T-064
Paths: `crates/ss-server/src/http/bots.rs`, `crates/ss-server/src/handlers/bots.rs`, `docs/bots.md`, `examples/bot-*`

## Goal
Members can write bots (music announcements, game stats, dice, reminders)
in any language without touching QUIC.

## Scope
- Bot accounts (`users.kind = 1`) created by `MANAGE_SERVER`; token issued
  once (hashed); roles assignable like users; appear in member list with BOT
  tag; cannot join voice in v1.
- Two transports for bots: (a) the native QUIC protocol with `Hello{bot_token}`
  (Rust bots use `ss-client-core` directly); (b) HTTPS REST for actions
  (`POST /api/v1/channels/{id}/messages`, edit/delete, DM, fetch members/
  channels) plus a WebSocket `/api/v1/events` streaming the same
  `ServerEnvelope` events as JSON for languages without QUIC.
- JSON mapping of protobuf (`prost-serde`/`pbjson`) so REST and WS share
  the schema with the proto docs.
- Scopes on bot tokens (subset of permissions) and per-bot rate limits.
- Slash-command-lite: messages starting with `/` are not special server-side;
  provide a `MessageCreated` filter param `prefix=` on the WS subscription so
  simple bots don't get everything.
- `examples/bot-python/` (REST+WS, 60 lines) and `examples/bot-rust/`
  (native) that echo and roll dice.
- `docs/bots.md` with auth, endpoints, events, examples.

## Out of scope
Outgoing webhooks, interactions/buttons UI, bots in voice.

## Acceptance criteria
- Both example bots run against a test server in CI.
- Permission tests: bot without `SEND_MESSAGES` in a channel is denied via
  both transports.

## Notes from implementation

Implemented 2026-11-12.

**What exists**

- Bot accounts are `users` rows with kind 1 and no usable password
  (`password_hash` is `!bot`, not a PHC string; login treats a bot's name as
  unknown and still runs the dummy hash). Tokens live in a new table
  `bot_tokens` (migration 0008): 32 random bytes, base64url, SHA-256 hex
  stored, at most one live token per bot (partial unique index), `scopes`,
  `created_by` (NULL for the operator CLI), `revoked_at`. Shown once, the
  webhook pattern.
- Management: `CreateBot` (46), `ResetBotToken` (47, new token, old one
  revoked), `RevokeBotToken` (48), `ListBots` (49), answered with
  `Ack.bot` (14, the only place the plaintext appears) or `Ack.bots` (15).
  They need `MANAGE_SERVER`; a bot can never run them; scopes must be known
  bits that the issuer holds server-wide; reset and revoke also need
  `can_act_on` over the bot. Each writes an audit row (`bot.create`,
  `bot.token.reset`, `bot.token.revoke`). The operator CLI does the same
  without the member checks: `ss-server bot create|list|reset|revoke`
  (`--scopes VIEW_CHANNEL,SEND_MESSAGES` by name). Roles are assigned with
  the existing `SetUserRoles`. Creation publishes `MemberJoined`. Welcome
  feature `bots`.
- Scopes: `PermissionService` keeps every bot's live token id and scopes in
  its snapshot and caps every resolution (server-wide, per channel,
  `is_administrator`) to role bits ∩ scopes, minus `CONNECT|SPEAK|STREAM`
  (`perms::BOT_DENIED`); `ADMINISTRATOR` survives only when both grant it.
  Commands, visibility filters, the channel-view diff and the Welcome's
  `effective_permissions` all read the capped set, so the cap cannot be
  missed by one path. A bot without a live token has empty scopes.
- No voice: the cap, and `JoinVoice` refuses bots ("bots cannot join
  voice") before any other check.
- Native QUIC: `Hello.auth.bot_token = 9`. The connection is attached to
  `auth::Sessions` under the token row's id, so a revoke closes it like a
  session (`SessionRevoked`, code 6) and the 30 s sweep also checks
  `bot_tokens`. `Welcome.resume_token` echoes the bot token.
  `ss-client-core` gained `Credentials::Bot`: reconnects send `bot_token`,
  the token is never written to `Storage`, and the post-reconnect history
  re-read is skipped (it needs a session bearer). Typed `create_bot`,
  `reset_bot_token`, `revoke_bot_token`, `list_bots`.
- HTTPS (`http/bots.rs`, `Authorization: Bot <token>`): `POST
  /api/v1/channels/{id}/messages`, `PATCH` and `DELETE
  /api/v1/messages/{id}`, `POST /api/v1/dms`, `GET /api/v1/members` (pages
  of ≤ 1000 by id), `GET /api/v1/channels` (visible guild channels with
  effective permissions, plus the first DM page), `GET /api/v1/events`.
  An action builds the QUIC command and runs `connection::replies` and
  `publish` on its own task (`connection/bots.rs`), so permissions, scopes,
  quotas, the T-113 bus hold and the events are exactly the QUIC ones; the
  REST call gets a fresh origin id, so the bot's own stream and QUIC sessions
  see its actions. Replies are the `Ack` result as JSON, `204` for `Empty`;
  errors map the protocol code to a status with `Retry-After` for quotas.
  Path ids are ULIDs or base64url.
- Credentials never cross: `Hello.bot_token` and the `Bot` scheme look only
  in `bot_tokens`; `session_token`, `resume_token` and `Bearer` only in
  `sessions`, and a session row naming a bot account is refused anyway.
  The bot routes refuse `Bearer`; the member routes never accept `Bot`.

**Transport choice: SSE, not WebSocket.** axum 0.8.9's `ws` feature pulls
`tokio-tungstenite` and `tungstenite` (and their helpers), none of which are
in the graph today. Server-sent events are in axum already
(`axum::response::sse`), run on the existing HTTPS listener and its
hardening, and every language's HTTP client can read them (the Python
example uses `http.client`). A bot only needs server→client events; actions
go over REST. So `GET /api/v1/events` is `text/event-stream`, one `data:`
line per `ServerEnvelope`, keep-alive comments every 15 s (under the 60 s
idle close). The stream ends itself after 9 minutes, below the listener's
10-minute connection age, and the client reconnects for a new `welcome`.

**JSON choice: `pbjson` 0.9 + `pbjson-build` 0.9 (MIT, influxdata).** The
task asked for the protobuf JSON mapping so REST, the stream and the proto
docs share one schema. The repository had only hand-written JSON for a few
types (`http/messages.rs`); hand-writing every `ServerEnvelope` variant and
the `Welcome` snapshot would be several hundred lines that drift from the
schema with every new field. `pbjson-build` generates `serde` impls from
the same descriptors prost uses, in the canonical proto3 JSON mapping, which
is exactly the format `02-protocol.md` already promised for history
(camelCase, base64 bytes, decimal int64, enum names); the new
`ss-proto/tests/json.rs` pins that. It is behind ss-proto's `json` feature,
which only ss-server enables, so clients do not compile it, and it is set
to ignore unknown fields on input (T-101). New crates in the lockfile: only
`pbjson` and `pbjson-build`; their dependencies (`base64` 0.22, `serde`,
`heck`, `itertools`, `prost-types`) were already in the graph. `cargo deny
--all-features check` passes.

**Event visibility.** The stream mirrors `connection::on_event` in
`connection/bots.rs` (`forward`): DM events applied by membership, the
debounced guild channel diff (a channel becomes visible with
`channelCreated` before anything in it), `announce_direct`,
`snapshot::delivers`, and the voice-state gate. It reuses the connection's
`View`, `sync_channels`, `apply_direct` and `announce_direct`; only the
gate is a local copy, because the stream is not a registered connection
(the registry's `admit_voice` refuses unknown connections). It subscribes
at the snapshot's read point via `snapshot::build_live`, like a sign-in. It
never drops a frame: a client that does not read for 10 s, or a stream 1024
events behind, gets `serverNotice` and the end, as a QUIC session does.
`?prefix=` (1–32 characters) passes only `messageCreated` events whose
content starts with it; other kinds pass.

**Revocation.** `RevokeBotToken`/`ResetBotToken` revoke in the write
transaction, reload permissions, then close the token's QUIC connections
and signal its streams (a `watch` per token), which send `sessionRevoked`
and end. Every REST request reads the row, so the next one is `401`. The
CLI changes rows from another process: the sweep closes QUIC connections
within 30 s, streams recheck their token and account every 30 s (which also
catches a ban), and an HTTPS request whose token the permission snapshot
does not cap by yet reloads it first and is refused if it still does not
match, so a CLI-created bot is never served uncapped.

**Limits.** Per bot: 50 requests, then 5 a second (`limits::BOT_REQUEST_*`),
spent in `replies` by every QUIC command and REST action and by each REST
read; on top of the per-account command budget (also spent by REST
actions), `SendMessage`'s and `CreateDm`'s own quotas, the per-address HTTP
quota and connection caps. Two open streams per token
(`limits::BOT_STREAMS`). Bodies 64 KiB like every route. Bots have no
upload or download route, so the T-092 transfer caps are not reachable.

**Desktop.** `MemberView.bot` (ts-rs) and a BOT tag in `MemberRow`, with a
vitest. No bot management UI.

**Tests** (`crates/ss-server/tests/bots.rs`, 13). Four guards were
mutation-checked: with the scope cap, the CLI reload, the stream's
visibility filter or the stream revoke disabled, their tests failed.

| Property | Test |
|---|---|
| Bot token never a session, or the reverse, on QUIC and HTTPS; a bot's name cannot log in | `a_bot_token_and_a_session_token_never_stand_in_for_each_other` |
| Revoke refuses at once: open stream ends with `sessionRevoked`, QUIC closed as signed out, REST `401`, `Hello` refused; reset kills the old token | `a_revoked_token_is_refused_at_once_on_every_transport` |
| Scopes and overrides deny `SendMessage` on both transports; an Admin role without the scope is not admin | `scopes_and_overrides_limit_a_bot_on_both_transports` |
| A CLI-made bot is capped at once; CLI reset revokes; the sweep closes | `a_bot_the_operator_cli_creates_is_capped_and_revoked_like_any_other` |
| Only managers create; no scope they lack; look-alike names refused; bots cannot manage bots | `only_managers_create_bots_and_only_with_bits_they_hold` |
| No voice, even as an all-scopes administrator | `a_bot_cannot_join_voice` |
| Stream hides a channel the bot cannot view, then announces it when granted | `the_stream_never_shows_a_bot_a_channel_it_cannot_view` |
| Per-bot budget across HTTPS and QUIC; members unaffected | `per_bot_limits_apply_across_transports` |
| Two streams per bot; a closed one frees its slot | `a_bot_holds_at_most_two_event_streams` |
| REST send/edit/delete/DM, prefix filter, own echo | `rest_actions_and_reads_share_the_protobuf_json_schema` |
| Members and channels reads | `rest_reads_page_members_and_list_visible_channels` |
| Rust example answers over QUIC | `the_rust_example_answers_over_quic` |
| Python example answers over real HTTPS (skips without `python3`) | `the_python_example_answers_over_https` |

`just check` passes (nightly rustfmt, clippy `-D warnings`, 758 tests
passed and 6 ignored in 63 binaries, `just plan-check`);
`tests/bots.rs` passed 8 runs in a row. `cargo clippy -p
socialspeak-desktop --all-targets -- -D warnings`, `cargo test -p
socialspeak-desktop`, `cargo deny --all-features check`, and the frontend
`tsc -b`, `eslint .`, `prettier --check .` and `vitest run` (156 tests) pass.

The authorization audit lists the four commands, the seven routes, the
stream's filter and the HTTPS reload. These tests were written alongside the
code rather than strictly before it; the mutation runs stand in for the
red step.

**Files outside the Paths**
- Protocol: `crates/ss-proto/proto/socialspeak/v1/{auth,envelope}.proto`
  (new tags only), new `bots.proto`, `crates/ss-proto/{Cargo.toml,build.rs,
  src/lib.rs,README.md}`, new `crates/ss-proto/tests/json.rs`,
  `docs/protocol/messages.md` (generated).
- Server: `migrations/0008_bot_tokens.sql`, new `src/db/bots.rs`,
  `src/db/mod.rs`, `src/perms.rs` (the cap), `src/auth/{mod,live}.rs`
  (bot sign-in, login skips bots, sweep), `src/limits.rs`,
  `src/voice/session.rs` (one guard in `join`), `src/handlers/mod.rs`,
  `src/http/mod.rs`, `src/main.rs` (`bot` CLI), new
  `src/connection/bots.rs`, `Cargo.toml`, `README.md`,
  `tests/authorization_audit.rs`.
- `src/connection.rs`: the `bots` field and its construction, the
  `BotToken` arm in `scrub`, the per-bot budget and the `bots.dispatch`
  branch in `replies` (plus a `bot_budget` helper), `public_routes` takes
  `self: &Arc<Self>` and merges `http::bots::router`, `mod bots`, and the
  `bots` feature string. Nothing in `session`, `prepare_welcome`,
  `read_inbound`, the registry or the Welcome path (T-116's area) changed.
- Client: `crates/ss-client-core/src/{connection,commands}.rs`, `README.md`.
- Desktop: `apps/desktop/src-tauri/src/voice_view.rs`,
  `apps/desktop/src/lib/generated/MemberView.ts`,
  `apps/desktop/src/features/channels/{MemberRow.tsx,ChannelWorkspace.test.tsx}`,
  `apps/desktop/src/features/dm/dm.test.tsx` (fixture field),
  `apps/desktop/src/styles.css`.
- Workspace: root `Cargo.toml` (`pbjson`, `pbjson-build`,
  `examples/bot-rust` member), `Cargo.lock`.
- Docs: `docs/plan/02-protocol.md`, `docs/plan/03-data-model.md`,
  `docs/security.md`, `docs/protocol/README.md`, `docs/self-hosting.md`
  (one table row; T-115 edits this file too), this file, `INDEX.md`.

**Deferred**
- Bot uploads and attachment downloads, and history reads with a bot token
  (`GET .../messages` still takes only a session bearer). A bot learns
  messages from the stream.
- Replay across streams (an event id and a resume cursor); a reconnect gets
  a fresh `welcome` and misses the gap.
- A bot management screen in the desktop app.
- A bot with only an HTTPS stream shows offline: presence comes from
  registered QUIC connections.

**Open questions**
- `Logout` on a bot's QUIC connection closes that bot token's connections
  with code 6, which tells `ss-client-core` not to reconnect, but does not
  revoke the token. Refuse it for bots instead?
- A ban ends a bot's open streams only at their 30 s recheck, because
  moderation does not know about streams; its requests are refused at once.
- Should bot tokens expire? They do not; revocation is the remedy, as for
  webhooks.
