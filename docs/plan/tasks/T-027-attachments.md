# T-027 — Attachments: upload, storage, serving, image metadata

Status: todo
Phase: 2
Size: M
Depends on: T-026
Paths: `crates/ss-server/src/http/attachments.rs`, `crates/ss-server/src/storage.rs`

## Goal
Users can attach files and images to messages; images show inline with
correct dimensions.

## Scope
- `POST /api/v1/attachments` multipart, bearer auth, `ATTACH_FILES` in the
  target channel (channel id as a field), size limit from config (default
  25 MiB), per-user daily quota (default 500 MiB).
- Content sniffing with `infer`; reject executables by default
  (configurable allowlist); normalize filename; store at
  `attachments/<yy>/<mm>/<ulid>` with sha256 dedupe (same hash → reuse blob,
  new attachment row).
- Image metadata via `image` crate in `spawn_blocking`: width/height; EXIF
  orientation respected; strip EXIF from a generated preview? **No**
  re-encoding in v1; just metadata. Thumbnails are a backlog item.
- `GET /api/v1/attachments/{id}/{filename}`: auth via bearer **or** a short-
  lived signed query token (so `<img>` tags in the webview work); `Content-
  Disposition: attachment` for non-image/non-video types; strong ETag.
- Orphan GC: attachments not linked to a message within 1 h are deleted by a
  background task.
- Avatars reuse attachments (`UpdateProfile.avatar_attachment_id`).

## Out of scope
Thumbnails, video transcoding, external object storage.

## Acceptance criteria
- Tests: upload → attach to message → download with bearer and with signed
  token; oversize rejected; disallowed type rejected; dedupe works; orphan GC
  removes unlinked files.

## Notes from implementation
