# T-027 — Attachments: upload, storage, serving, image metadata

Status: done
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

- `POST /api/v1/attachments` takes multipart fields `channel_id` (Crockford
  ULID) and `file`. The JSON body uses camelCase. `attachmentId` is standard
  base64 of the 16 raw bytes. `url` is a path, not an absolute URL, because
  the process does not bind HTTP yet: `/api/v1/attachments/{id}/{filename}?token=`.
  The id in that path is Crockford. `size` is a decimal string.
- Defaults live on `storage::UploadPolicy`: 25 MiB per file, 500 MiB per user
  over a rolling 24 hours (each new row counts, including a deduped blob),
  orphan window 1 hour, signed token lifetime 10 minutes. `executable_allow`
  is a list of MIME types or extensions.
- `infer` supplies the media type. Client `Content-Type` is ignored. PE, ELF,
  Mach-O, wasm, dex, and Java class files are rejected, as are names ending in
  `exe`, `dll`, `so`, `dylib`, `com`, `scr`, `msi`, `sys`, or `wasm`, unless
  the allowlist names that MIME type or extension. The stored name is the last
  path segment, with control characters replaced, at most 200 characters.
- Bytes are stored at `attachments/<yy>/<mm>/<ulid>` (UTC, two-digit year and
  month). The same SHA-256 reuses the existing blob and still inserts a row.
  Image width and height come from the `image` crate on a blocking thread.
  EXIF orientations 5–8 swap the axes. The original bytes are not re-encoded.
- `GET` accepts `Authorization: Bearer` or a valid `token` query. The token is
  HMAC-SHA256 over the id and expiry, using `data-dir/attachment.key`. A valid
  token is enough. Otherwise a linked file requires `VIEW_CHANNEL` on its
  message's channel, an unlinked file requires the uploader, and an avatar or
  the server icon is readable by any signed-in user. Missing view is 404.
  Missing `ATTACH_FILES` is 403 `missing attach files`. Voice and category
  channels are 400 `not a text channel`. Images and video use inline
  `Content-Disposition`; other types use `attachment`. The ETag is the quoted
  SHA-256 hex. `If-None-Match` returns 304.
- Orphan cleanup runs every 5 minutes and on `Store::collect_orphans`. It
  deletes rows with no message that are older than an hour, except avatars
  and the `icon` setting. A shared blob is removed only when no row still
  points at it.
- `UpdateProfile` keeps the display name when the field is empty (otherwise
  at most 64 characters). Empty avatar bytes clear the picture. A 16-byte id
  must be a file that user uploaded. The ack is the user, and `MemberUpdated`
  is fanned out.
- `connection::Server::files()` owns the store. `http::attachments::router`
  is what tests mount. The binary still does not listen for HTTP; that socket
  stays with the operations task. No new migration.
