# T-045C — Chat attachments and previews

Status: in-progress
Phase: 3
Size: M
Depends on: T-045A, T-027, T-029
Parent: T-045
Paths: `apps/desktop/src/features/chat/`

## Scope
Complete T-045's attachment and link-card UI: paste/drop uploads with progress,
image grid/lightbox, accessible image labels, filenames/sizes/downloads, inline
video/audio, link previews and hide behavior. Keep authenticated and pinned
network operations in the native layer. Coordinate rendering with T-045B.

## Acceptance
The corresponding T-045 criteria pass with permission/error/cancellation cases,
accessible controls, and native authenticated downloads/uploads; `just check`
and `just check-desktop` pass. Record cross-platform media acceptance separately.

## Implementation — 2026-11-04

Implemented file picker, paste and drop uploads, preparation/upload activity
indicators, cancellation, retry and per-account/server/channel attachment drafts.
Messages can contain files without text; the server still rejects entirely empty
posts. Up to 10 files, each 1 byte–25 MiB, are accepted by the desktop; server
limits, types, permissions and quotas remain authoritative. Files in one batch
upload sequentially to bound temporary encoding buffers. Cancelled or abandoned
uploads remain subject to the server's existing orphan cleanup.

Attachment cards expose filenames/sizes, explicit image/audio/video loading,
keyboard-dismissable image lightboxes and native downloads. Downloads are saved
to `Downloads/Sideband downloads` with collision-resistant, sanitized filenames
and never overwrite existing files. Media uses revocable blob URLs; the webview
never receives bearer tokens or fetches from server attachment URLs directly.
Native transfers use the approved QUIC certificate pin, authenticated HTTPS,
bounded response bodies, deadlines and no redirects or environment proxies.

Link cards include title, description, site and session-local hide/show. External
preview images load only on explicit request, with an IP-sharing notice. Those
credential-free requests allow public HTTPS hosts on port 443, pin validated DNS
answers, reject redirects, accept raster image MIME types and cap bodies at 4 MiB.
The public-address policy is shared with the existing server preview fetcher.
SVG/HTML are not rendered as attachment media. OS/webview codec support determines
which inline audio/video formats play; downloads remain available.

Validation covers real pinned HTTPS upload/download, wrong-certificate rejection,
private orphan access, attachment-only posting, bounds, invalid filenames,
private preview hosts, and frontend permission/cancellation/retry/draft behavior.
Browser checks with a mocked native bridge passed at 1200×900 and 760×600: image
load/decoding/lightbox/Escape, native-download dispatch, opt-in preview image,
hide/show, failed upload/retry, file-only send, drop/remove and cancellation.
`just check` and `just check-desktop` passed (68 frontend tests and 53 desktop
native tests); the production frontend build passed with the existing chunk-size
warning. An initial full run hit the unrelated voice-resume timing test; both its
targeted rerun and the final complete workspace run passed.

Remaining acceptance: installed Windows/macOS/Linux file picking and drag/drop,
clipboard files, actual Downloads placement, and supported audio/video codec
playback. Cross-platform manual evidence is still required before closing this
task and the parent T-045 performance/acceptance gate.
