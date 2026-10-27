# T-029 — Server-side link previews with SSRF protection

Status: done
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

- The first three distinct http(s) URLs are fetched. `<url>` is skipped.
  Fragments and userinfo are dropped. The cache key is the normalized URL.
- Each hop is pinned to one resolved address. Loopback, private, link-local,
  multicast, CGNAT, and IPv4-mapped copies of those are refused before the
  socket opens. Redirects are followed by hand, at most three, and checked
  again. The body stops at 1 MiB. The image URL is stored and not proxied.
- Success is cached for 24 hours and failure for 1 hour. A later message with
  the same URL does not fetch again. `MessageUpdated` carries the ready cards.
  `MessageCreated` is still empty because the fetch happens after the ack.
- Settings are read once when the workers start. `link_previews=off` disables
  them. `link_preview_allow` and `link_preview_deny` are comma-separated hosts.
  `*.example.com` matches a suffix. Tests may allow loopback; that switch is
  not read from settings.
