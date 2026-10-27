# T-026 — Text messaging: send/edit/delete, replies, mentions, history, read states

Status: done
Phase: 2
Size: M
Depends on: T-023, T-024
Paths: `crates/ss-server/src/handlers/messages.rs`, `crates/ss-server/src/http/messages.rs`

## Goal
Persistent text channels with the core Discord message semantics.

## Scope
- `SendMessage`: `SEND_MESSAGES` (+ `ATTACH_FILES` if attachments), content
  1–4000 chars after trim, `reply_to` must be in the same channel, attachments
  must belong to the sender and be unattached; parse mentions `<@ULID>` and
  `@everyone` (requires `MENTION_EVERYONE`); store; emit `MessageCreated`
  with the client `nonce`; enqueue link preview job (T-029 hook).
- `EditMessage`: author only, within 7 days; `edited_at`; `MessageUpdated`.
- `DeleteMessage`: author or `MANAGE_MESSAGES`; soft delete; `MessageDeleted`.
- `Typing`: fan out `TypingStarted`, server-side rate limit 1/5 s per user
  per channel.
- `MarkRead`: upsert read state, `ReadStateUpdated` to the user's other
  connections; `mention_count` computed per channel for the snapshot.
- REST: `GET /channels/{id}/messages?before&limit` (max 100) and
  `/around/{id}`, both permission-checked, returning the same `Message`
  protobuf serialized as JSON (prost-serde or a hand mapping).
- Per-user send rate limit 5 msgs / 5 s.
- Markdown is rendered client-side; the server stores raw text.

## Out of scope
Attachments upload (T-027), previews (T-029), search (T-065), reactions/pins
(backlog).

## Acceptance criteria
- Integration tests for every command including permission denials, reply
  cross-channel rejection, edit window, soft-delete visibility in history
  (deleted messages are omitted), pagination correctness at boundaries,
  mention parsing and `mention_count`.

## Notes from implementation

- `SendMessage` needs `VIEW_CHANNEL` and `SEND_MESSAGES` on a text, DM, or
  group-DM channel. Content is trimmed and must be 1–4000 characters. A reply
  must be a live message in the same channel. Attachment ids must already
  belong to the sender and be unattached, and they also require
  `ATTACH_FILES`. `<@ULID>` mentions are stored when that user exists.
  `@everyone` requires `MENTION_EVERYONE` and is recorded on
  `messages.mentions_everyone` (migration `0002`). The ack is the message id.
  `MessageCreated` echoes `nonce`. A preview job is queued and discarded
  until T-029 starts workers.
- `EditMessage` is the author only, within 7 days, and emits `MessageUpdated`.
  `DeleteMessage` is the author or `MANAGE_MESSAGES`, soft-deletes the row,
  and emits `MessageDeleted`. History pages omit deleted rows.
- `Typing` emits `TypingStarted` at most once per 5 seconds per user per
  channel. Sends are limited to 5 per 5 seconds per user. Both return
  `RATE_LIMITED` when the quota is spent.
- `MarkRead` upserts the cursor. `ReadStateUpdated` goes to the actor and
  that user's other connections, not the whole server. `mention_count` is
  the number of later visible messages that ping the user or `@everyone`,
  excluding messages that user wrote. `Welcome` fills the same count.
- Message and typing events are channel events, so `snapshot::delivers`
  hides them from someone who cannot view the channel.
- History is `GET /api/v1/channels/{id}/messages?before&limit` and
  `.../around/{message_id}`, authorized with `Authorization: Bearer` and the
  session token. Limit defaults to 50 and rejects values above 100. The JSON
  is protobuf JSON (base64 ids, camelCase, int64 as strings) in ascending id
  order. The routes are `http::messages::router`. The process does not bind
  HTTP yet; that listen socket belongs with the operations task. `Welcome`
  features include `messages`.
