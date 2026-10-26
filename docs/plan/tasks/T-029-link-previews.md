# T-029 — Server-side link previews with SSRF protection

Status: todo
Phase: 2
Size: M
Depends on: T-026
Paths: `crates/ss-server/src/previews/`

## Goal
Messages containing URLs get OpenGraph/oEmbed-style preview cards, fetched by
the server so members' IPs never touch the target.

## Scope
- URL extraction on `SendMessage` (first 3 http(s) URLs); dedupe by
  normalized URL hash; cache in `link_previews` with TTL 24 h (negative cache
  1 h).
- Fetcher with `reqwest`: custom DNS resolve → reject if any resolved IP is
  private/loopback/link-local/multicast/CGNAT; connect to the resolved IP
  (pin via `resolve()`), 5 s total timeout, 1 MiB body cap, `Accept:
  text/html`, follow ≤ 3 redirects re-validating each hop, UA string
  identifying the server.
- Parser: `<meta property="og:*">`, `twitter:*`, `<title>`, `<meta
  name=description>`, favicon; oEmbed discovery link for YouTube-like sites
  (fetch JSON, same guards). Image URL is stored, **not** proxied in v1
  (clients load it directly; a proxy is a backlog item with a privacy note).
- Job queue: bounded `mpsc`, 2 workers, per-host concurrency 1; results
  update `message_links` and emit `MessageUpdated` with `link_previews[]`.
- Users can suppress previews by wrapping a URL in `<…>` (Discord
  convention).
- Config: enable/disable, allow/deny host lists.

## Out of scope
Image proxying, embeds for arbitrary sites beyond OG/oEmbed.

## Acceptance criteria
- Tests with a local mock HTTP server: OG parse, redirect chain to a private
  IP rejected, oversized body truncated, DNS to 127.0.0.1 rejected, cache hit
  avoids refetch, `<url>` suppression.

## Notes from implementation
