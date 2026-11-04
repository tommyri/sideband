# T-045B — Rich text and composer completion

Status: in-progress
Phase: 3
Size: M
Depends on: T-045A
Parent: T-045
Paths: `apps/desktop/src/features/chat/`

## Scope
Complete the non-attachment requirements in T-045: sanitized Markdown and syntax
highlighting, spoilers, mentions (including canonical Crockford aliases and
@everyone), emoji/mention completion, typing indicators, markdown shortcuts,
Up-to-edit, full keyboard list navigation and focus management, copy link/id,
lookup of unloaded reply targets, and own-view deleted-message placeholders.
Preserve drafts across channel navigation. Extend rendering/XSS, mentions, and
accessibility tests. Verify the 10k-message performance target on representative
hardware before closing the parent task.

## Acceptance
The corresponding T-045 criteria pass, including adversarial Markdown/URL tests;
`just check` and `just check-desktop` pass. Document manual evidence separately.

## Implementation — 2026-11-04

Implemented sanitized Markdown, fenced-code highlighting, revealable spoilers,
canonical member mentions, @everyone, a built-in emoji catalog and keyboard
completion. HTTP(S) links open through the native bridge after URL validation;
remote Markdown images do not load. Composer formatting shortcuts, Up-to-edit,
scoped typing indicators, session-only text drafts and stable retry nonces are
shared by channels and DMs. Message rows support keyboard navigation, copy
text/ID/link and pinned-HTTPS lookup of replies outside the loaded cache.
Own-message deletion preserves an erased marker for deletions observed during
this session; other users' deleted messages disappear.

Copied `sideband://message/...` links navigate when opened inside Sideband chat.
OS-level protocol registration is not included. Draft text is isolated by server,
account and conversation, retained across navigation, and not persisted to disk.

Validation: rendering/URL safety, completion, typing expiry/throttle, draft and
nonce isolation tests; pinned-HTTPS around-message integration and erased-marker
regression tests. Local browser interaction checks exercised reply lookup/focus,
Markdown/spoilers, native-link dispatch, draft recovery, mention completion,
Up-to-edit, typing, copy link and deletion, including sending at 760×600.
The browser harness mocked the native bridge; it does not replace installed-app
acceptance. `just check-desktop` passed (63 frontend tests, 49 native tests and
strict linting). `just check` passed across the workspace. The production
frontend build passed with a bundle-size warning.

Remaining acceptance: measure 10k-message scrolling at 60 fps on representative
hardware and verify installed-app keyboard/native-link behavior across platforms.
Keep this task and T-045 open until that evidence is recorded.
