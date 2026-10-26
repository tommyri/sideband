# T-065 — Message search (FTS5)

Status: todo
Phase: 4
Size: S
Depends on: T-026, T-045
Paths: `crates/ss-server/migrations/00xx_fts.sql`, `crates/ss-server/src/http/search.rs`, `apps/desktop/src/features/search/`

## Goal
Find that link someone posted three weeks ago.

## Scope
- FTS5 external-content table over `messages(content)` with triggers;
  tokenizer `unicode61 remove_diacritics 2`.
- `GET /api/v1/search?q=&channel=&from=&before=&after=&has=link|file|image
  &limit=` scoped to channels the user can view (and their DMs); results
  ranked by bm25 then recency; highlights.
- Client: search box in the header, results pane with jump-to-message
  (T-045 jump).
- Reindex CLI command.

## Out of scope
Attachment content indexing.

## Acceptance criteria
- Permission test: results never include invisible channels.
- 100k-message fixture search returns < 50 ms.

## Notes from implementation
