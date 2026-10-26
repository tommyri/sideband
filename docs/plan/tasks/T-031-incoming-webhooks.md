# T-031 — Incoming webhooks

Status: todo
Phase: 2
Size: S
Depends on: T-026
Paths: `crates/ss-server/src/http/webhooks.rs`, `crates/ss-server/src/handlers/webhooks.rs`

## Goal
External systems can post messages into a text channel with a URL + token,
Discord-compatible enough that existing integrations mostly work.

## Scope
- `CreateWebhook{channel_id, name}` / `DeleteWebhook` (`MANAGE_SERVER`);
  token shown once; stored hashed. Add messages to proto.
- `POST /api/v1/webhooks/{id}/{token}` with JSON `{content, username?,
  avatar_url?}`; also accept Discord's `embeds[0].title/description` by
  flattening into content (best effort). No bearer; rate limit 30/min per
  webhook; 2000 char cap.
- Messages have `author_id = NULL`, `webhook_id` set, display name from
  payload or webhook name; rendered by clients with a "BOT" tag.
- Audit on create/delete.

## Out of scope
Outgoing webhooks, full bot API (T-080).

## Acceptance criteria
- Tests: create → post → `MessageCreated` fan-out; wrong token 404; revoked
  token 404; rate limit; a sample GitHub/Grafana-style payload yields a
  readable message.

## Notes from implementation
