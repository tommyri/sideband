# T-080 — Bot API: bot accounts, tokens, REST + event stream, docs

Status: todo
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
