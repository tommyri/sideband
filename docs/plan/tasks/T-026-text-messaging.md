# T-026 — Text messaging: send/edit/delete, replies, mentions, history, read states

Status: todo
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
