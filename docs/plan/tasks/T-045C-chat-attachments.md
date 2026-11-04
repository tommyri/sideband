# T-045C — Chat attachments and previews

Status: todo
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
