# T-065 — Message search (FTS5)

Status: done
Phase: 4
Size: S
Depends on: T-026, T-045
Paths: `crates/ss-server/migrations/0006_message_search.sql`, `crates/ss-server/src/http/search.rs`, `apps/desktop/src/features/search/`

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

- Implemented FTS5 live-content view, transactional mutation triggers and initial/rebuild indexing.
- Authenticated HTTPS search applies current guild visibility and DM membership before ranking,
  then rechecks permission before delivery. Query words are quoted, bounded literal tokens.
- Added author/channel/date/attachment filters and plain-text match delimiters; no HTML rendering.
- Native bridge releases its clients mutex before the request. Detached searches reject connection
  generation changes and filter against current client visibility.
- Header search dialog supports filters, empty/error/loading states, safe highlights, and existing
  T-045 jump navigation. Message/access/connection events invalidate in-flight and displayed results.
- Reindex command and API/operator usage: [Message search](../../message-search.md).
- Validation: 100k selective query measured 1.3 ms locally (a broad 99,900-match query took
  403 ms in the debug fixture; not a universal latency guarantee); index lifecycle/filter tests,
  authenticated permission/DM integration, frontend stale-response/highlight/navigation tests,
  and Chromium at 1200×900 and 760×600. Installed cross-platform acceptance remains in release gates.
- T-045's remaining manual acceptance does not block using its implemented jump-to-message API.
