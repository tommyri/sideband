# T-045 — Text chat UI

Status: todo
Phase: 3
Size: L
Depends on: T-042, T-027, T-029
Paths: `apps/desktop/src/features/chat/`

## Goal
Discord-grade text chat: fast scrolling, markdown, mentions, replies,
attachments, link cards, unread handling.

Split suggestion for sub-agents: (a) message list + virtualization +
history loading, (b) composer + markdown + mentions, (c) attachments +
previews + lightbox. Create sub-task files if splitting.

## Scope
- Message list: virtualized (`@tanstack/virtual`), grouped consecutive
  messages by author within 7 min, day separators, "new messages" divider at
  last read, scroll-anchoring when older pages load, jump-to-bottom button,
  jump-to-message for reply links.
- Rendering: markdown subset (bold/italic/strike/code/codeblock with
  highlighting/quotes/links/spoilers), `<@id>` mentions resolved to names with
  highlight (parse ids case-insensitively with Crockford aliases, as the
  server does; T-061 fuzzing showed bodies may spell them differently), `@everyone`, Unicode emoji shortcodes `:smile:` + picker, edited
  marker, deleted placeholder for own view only.
- Composer: multi-line (Shift+Enter), markdown shortcuts, `@` mention
  autocomplete, `:` emoji autocomplete, reply bar, edit-last-with-Up, paste/
  drag-drop files with upload progress, typing indicator send.
- Attachments: image grid with lightbox, file chips with size/download,
  video/audio inline players for supported types.
- Link preview cards (title, description, image, site) with hide button.
- Context menu: reply, edit, delete (confirm), copy text, copy link, copy id.
- Mark-as-read on viewport bottom reached; unread badges in sidebar.

## Out of scope
Reactions, pins, threads, search UI (backlog / T-065).

## Acceptance criteria
- 10k cached messages scroll at 60 fps on a mid laptop.
- Vitest tests for the markdown renderer (XSS: raw HTML never rendered),
  mention parser, grouping logic.
- Accessibility: list is navigable, images have alt text (filename), focus
  returns to composer after actions.

## Notes from implementation
