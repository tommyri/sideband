# T-045B — Rich text and composer completion

Status: todo
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
