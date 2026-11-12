# Writing a bot

A Sideband bot is an account of its own, marked as a bot in the member list.
It holds roles like any member, and it signs in with a **bot token** instead
of a password. It can talk two ways:

- **Native QUIC**, the protocol the desktop app speaks. Rust bots use
  `ss-client-core` with `Credentials::Bot`; see
  [`examples/bot-rust/`](../examples/bot-rust/).
- **HTTPS**: REST calls for actions and a server-sent event stream for
  events, for any language with an HTTP client. See
  [`examples/bot-python/`](../examples/bot-python/), which uses only the
  Python standard library.

Both examples answer `!echo <text>` and `!roll 2d6`. Bots cannot join voice
channels in this version.

## Creating a bot

A member with **Manage server** creates a bot with the `CreateBot` command,
or the server operator does it from the command line:

```sh
ss-server bot create --username dice --scopes VIEW_CHANNEL,SEND_MESSAGES
ss-server bot list
ss-server bot reset --username dice --scopes VIEW_CHANNEL,SEND_MESSAGES
ss-server bot revoke --username dice
```

`create` and `reset` print the token **once**. The server stores only its
SHA-256, so a lost token cannot be shown again: reset it. Treat it like a
password. Anyone who has it acts as the bot.

The bot appears in the member list straight away. Give it roles like any
member (`SetUserRoles`, or the Roles screen in the desktop app).

### Scopes

Every token has **scopes**: the permission bits it may use. The bot holds a
bit only when its roles grant it *and* the token's scopes include it. The
default scopes, `VIEW_CHANNEL,SEND_MESSAGES`, let a bot read and post where
its roles allow and nothing more, even if someone later gives it a powerful
role. A member can only issue scopes they hold themselves; the operator can
issue any. `ADMINISTRATOR` works for a bot only when both the role and the
scopes have it. `CONNECT`, `SPEAK` and `STREAM` never do.

The permission names are the ones in
[the data model](plan/03-data-model.md#permissions).

### Replacing or revoking a token

Bot tokens do not expire (maintainer decision, 2026-11-12): a token is valid
until it is reset or revoked, so an unattended bot keeps working.

`ResetBotToken` (or `ss-server bot reset`) issues a new token and revokes the
old one. `RevokeBotToken` (`ss-server bot revoke`) revokes it without a
replacement. A revoked token is refused on the next request. Open
connections and event streams that used it get `SessionRevoked` and close at
once when a member revoked it, and within 30 seconds when the operator CLI
did. A stream request that signed in just before a revoke gets `401` and no
`welcome`.

A kick or a ban from a moderator ends the bot's event streams at once, with
`{"kicked": …}` or `{"banned": …}` as the last event, as it closes its QUIC
connections. A kick is only a disconnect: the bot may reconnect. A ban
refuses every request while it lasts. A ban the operator CLI records is
picked up within 30 seconds. Losing a role or a channel override takes
effect at once: the stream sends `channelDeleted` for channels the bot can
no longer see and nothing from them afterwards.

## Signing in over QUIC

Send `Hello` with `bot_token` (field 9 of the `auth` oneof) instead of a
session token or a password. Everything else is the normal protocol in
[the protocol reference](protocol/README.md): the `Welcome` snapshot,
commands, events, `Ping`. `Welcome.resume_token` echoes the bot token; after
a dropped connection, sign in with `bot_token` again. A bot token never works
as `session_token` or `resume_token`, and a session token never works as
`bot_token`.

In Rust:

```rust
let client = Client::connect(address, Credentials::Bot(token), pins).await?;
```

`Logout`, `ListSessions` and `RevokeSession` are refused for bots
(`FORBIDDEN`): a bot has no sessions, and a `Logout` would only have told
the client to stop reconnecting while the token kept working. Revoke the
token with `RevokeBotToken`.

## HTTPS

Every request carries `Authorization: Bot <token>`, on the server's HTTPS
port (the one the desktop app uses for history). A session bearer
(`Authorization: Bearer …`) is refused on these routes, and a bot token is
refused on the member routes. Verify the certificate as the desktop app
does: a public certificate by hostname, or a self-signed one by its SHA-256
fingerprint (`ss-server fingerprint`), which the Python example pins with
`SIDEBAND_FINGERPRINT`.

Bodies and replies are the **canonical protobuf JSON** of the messages in
[the message reference](protocol/messages.md): field names in camelCase,
ids and other bytes in base64, 64-bit integers as decimal strings, enums by
name (`USER_KIND_BOT`). Fields with their default value are left out, and
unknown fields are ignored, so a bot written today keeps working when the
schema grows. Ids in a path are ULIDs or base64url (the base64 of an event,
with `-` and `_` for `+` and `/`).

| Method | Path | Body | Reply |
|---|---|---|---|
| POST | `/api/v1/channels/{channel_id}/messages` | `SendMessage`, without `channelId`: `{"content": "hi", "replyTo"?, "nonce"?}` | `{"messageId": "…"}` |
| PATCH | `/api/v1/messages/{message_id}` | `EditMessage`, without `messageId`: `{"content": "…"}` | `204` |
| DELETE | `/api/v1/messages/{message_id}` | — | `204` |
| POST | `/api/v1/dms` | `CreateDm`: `{"userIds": ["…"]}` | `{"dmChannel": Channel}`; post into it with the first route |
| GET | `/api/v1/members?after=&limit=` | — | `{"members": [Member], "next"?}`, by user id, up to 1000 a page |
| GET | `/api/v1/channels` | — | `{"channels": [Channel], "directMessages": [Channel]}`: what the bot can see, with `effectivePermissions` |
| GET | `/api/v1/events?prefix=` | — | Event stream, below |

Actions run exactly as the same QUIC command would, with the same checks
(the bot needs `SEND_MESSAGES` in the channel to post, only its own messages
can be edited, and so on) and the same events for everyone else. Bodies must
be `Content-Type: application/json` and at most 64 KiB. A refusal is the
HTTP status for the protocol error code with
`{"error": "…", "code": "ERROR_CODE_…"}`: `401` (no or a revoked token),
`403`, `404`, `400`, `409`, and `429` with `Retry-After` in seconds.

Bots cannot upload attachments in this version.

### The event stream

`GET /api/v1/events` answers `text/event-stream`. Each event is one `data:`
line holding one `ServerEnvelope` as JSON; lines starting with `:` are
keep-alives sent every 15 seconds. The first event is `{"welcome": …}`, the
same snapshot a QUIC sign-in gets (the token is not repeated in it). Then
come the events a QUIC session of the bot would get, through the same
filters: nothing from a channel the bot cannot view, a channel appears
(`channelCreated`) when the bot gains access, and disappears
(`channelDeleted`) when it loses it. The bot's own actions appear too.

```text
data: {"welcome":{"user":{"id":"AZq…","username":"dice","kind":"USER_KIND_BOT"},…}}

data: {"messageCreated":{"message":{"id":"AZq…","channelId":"AZp…","content":"!roll 2d6",…}}}
```

`?prefix=!` passes only the `messageCreated` events whose content starts
with `!` (1 to 32 characters); every other kind of event still arrives.

The stream ends:

- with `{"sessionRevoked": …}` when the token is revoked or replaced, or
  the account is disabled or banned outside the app. Stop; the token is
  dead.
- with `{"kicked": …}` or `{"banned": …}` when a moderator kicks or bans the
  bot. After a kick you may reconnect; after a ban, requests fail until it
  is lifted.
- with `{"serverNotice": …}` when the server could not confirm the token
  is still live (its 30-second check failed twice). Reconnect: a live token
  gets a new stream, a dead one `401`. Opening a stream at such a moment is
  `503` with `Retry-After`.
- with `{"serverNotice": …}` after 9 minutes, or when the bot reads too
  slowly and falls behind. Reconnect: the new stream starts with a fresh
  `welcome`. Events between two streams are not replayed.

A bot may hold two streams at once; a third is `429`.

## Limits

On top of the limits every member has (`docs/security.md` §6: 100 commands
then 20 a second per account, 5 messages per 5 seconds, and so on), each bot
has a budget of 50 requests, then 5 a second, shared by its QUIC commands and
its HTTPS calls. Over it, QUIC answers `RATE_LIMITED` with `retry_after_ms`
and HTTPS answers `429` with `Retry-After`. The per-address limits on
connections and requests apply as for anyone.

## Running the examples

```sh
# Python, standard library only
SIDEBAND_HOST=chat.example.org SIDEBAND_PORT=443 SIDEBAND_BOT_TOKEN=… \
  [SIDEBAND_FINGERPRINT=…] python3 examples/bot-python/bot.py

# Rust, native QUIC (SIDEBAND_PORT is the QUIC port here)
SIDEBAND_HOST=chat.example.org SIDEBAND_PORT=443 SIDEBAND_BOT_TOKEN=… \
  [SIDEBAND_FINGERPRINT=…] cargo run -p sideband-bot-example
```

`crates/ss-server/tests/bots.rs` runs both against a test server, the Python
one only where `python3` is installed.
