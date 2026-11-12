# Protocol reference

For people writing a bot, a second client or a tool that speaks to a Sideband
server. Two documents hold the facts; this page only says where to look.

| Document | What it is the source for |
|---|---|
| [Wire protocol v1](../plan/02-protocol.md) | Connection lifecycle and session tokens (§1), control-stream framing (§2), which message goes which way and what it does (§3), audio datagrams (§4), video streams (§5), the HTTPS API (§6), versioning and feature flags (§7) |
| [Message reference](messages.md) | Every `.proto` message, field, field number and enum value, with the comments from the schema. Generated; do not edit it by hand |
| [Security](../security.md) | Certificate trust (§3), authentication and tokens (§4), permissions (§5), rate limits (§6), input limits (§7) |
| [Message search](../message-search.md) | `GET /api/v1/search` parameters and response |

The schema itself is in
[`crates/ss-proto/proto/socialspeak/v1/`](../../crates/ss-proto/proto/socialspeak/v1/).
If the protocol document and the schema disagree, the schema is what the
server parses.

## The shape of a session

1. Open a QUIC connection with ALPN `socialspeak/1` and datagram support, to
   the server's UDP port. Verify the certificate the way the desktop app does:
   a public certificate by hostname, or a self-signed one by its SHA-256
   fingerprint pinned on first use (§1 and security §3).
2. Open one bidirectional stream. Every control message travels on it as a
   length-prefixed protobuf `ClientEnvelope` or `ServerEnvelope` (§2).
3. Send `Hello` with a login, an invite registration, a saved session token,
   or a bot token.
   The server answers `Welcome` with a full `Snapshot`, or an `Error` and a
   close.
4. Send commands with increasing `request_id`s. Each gets exactly one `Ack` or
   `Error` with the same id. Events arrive with `request_id = 0` at any time;
   keep reading them, or the server closes you as a slow consumer.
5. Send `Ping` when idle (the desktop sends one every 15 seconds); the idle
   timeout is 30 seconds. After a drop, reconnect with `Hello{resume_token}`.
6. History, uploads, attachments and search use HTTPS with
   `Authorization: Bearer <session token>` (§6). The bot API uses
   `Authorization: Bot <token>`.

Unknown fields and unknown `oneof` variants must be ignored, not treated as
errors: the schema only ever grows (§7). Check `Welcome` server features
before using a command an older server may not have.

## Bots

A bot is an account of its own with a bot token (T-080). It can speak this
protocol natively, signing in with `Hello{bot_token}`, or use the HTTPS bot
API: REST actions and a server-sent event stream of the same
`ServerEnvelope` events as protobuf JSON. [Writing a bot](../bots.md) covers
both, with a Python example (standard library only) and a Rust one
(`ss-client-core`). An **incoming webhook** (`POST
/api/v1/webhooks/{id}/{token}`, §6) still suits a script that only posts into
one channel.

`ss-cli` is not an example client for `ss-server run`: `ss-cli connect`
speaks only the unauthenticated test spike. `ss-cli load` does drive the real
server ([`crates/ss-cli/`](../../crates/ss-cli/)); `ss-client-core` is the
library the desktop app and the Rust example bot use.

## Regenerating the message reference

`messages.md` is generated from the `.proto` comments by
[`scripts/proto-doc.py`](../../scripts/proto-doc.py), a small Python
standard-library script, because `protoc-gen-doc` is not part of the
toolchain. After changing a `.proto` file:

```sh
just proto-doc
```

`just plan-check` (and so `just check`) fails while `messages.md` is stale.
The script accepts only the proto3 subset the schema uses and stops with the
file and line of anything else, so a new construct needs a change to the
script rather than being dropped.
