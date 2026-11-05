# Message search

Choose **Search messages** in the connected server's channel header. Search words
must all appear in a message. Search is case-insensitive and ignores diacritics;
quotes, `OR`, `*`, and other punctuation do not enable advanced query syntax.
Use Channel, From, After/Before (exclusive UTC midnight boundaries), or Contains
(link, file, image) to narrow the newest 50 matches. Select a result to open its
conversation at that message. Attachment contents are not indexed.

Search includes readable text channels and DMs you belong to, including group
DMs. Administrator status does not reveal other people's DMs. Edits and deletions
update the index transactionally. The desktop clears results when messages,
permissions, or connection state change; search again to see current matches.
Searches and result snippets are not stored in the client's message cache.

## Server API

`GET /api/v1/search` requires the authenticated session bearer token. Parameters:

| Parameter | Meaning |
| --- | --- |
| `q` | Required, 1–16 literal words, at most 256 characters |
| `channel` | Optional conversation ULID |
| `from` | Optional author account ULID (webhooks have no author account) |
| `before`, `after` | Exclusive UTC Unix millisecond bounds |
| `has` | `link`, `file`, or `image` |
| `limit` | 1–100, default 50 |

The response is an array of `{id, channelId, authorId, authorName, createdAt,
excerpt}`. IDs and millisecond timestamps are strings; `authorId` can be null.
`excerpt` is plain text: U+0001 and U+0002 delimit highlighted matches. Never
interpret it as HTML. Results are ordered by timestamp and ID descending (newest first). Global FTS5
bm25 statistics are deliberately not used: changes in hidden channels or private
DMs must not change the order of visible results.
Visibility is applied before limiting and rechecked before delivery. Unknown or
inaccessible channel filters return an empty array. Invalid queries return 400;
missing or invalid credentials return 401. No hidden result count is returned.

## Rebuild the index

```sh
ss-server reindex-search --data-dir ./data
```

Use the same data directory as the server. The command rebuilds the FTS index from
live message text in a SQLite transaction, excluding soft-deleted messages.
A database backup includes the index; migration 0006 also indexes existing live
messages automatically. Rebuilding large databases holds the SQLite write lock,
so schedule maintenance when chat traffic is low.

## Validation

`cargo test -p ss-server --test search -- --nocapture` exercises insert, edit,
soft/hard delete, malformed queries, filters, rebuild, and a 100,000-message fixture.
The selective two-word query (100 matches) measured 1.1 ms locally in a debug build;
a common-term query matching 99,900 rows took 374 ms in the same debug fixture.
These are reproducible fixture measurements, not universal bounds or end-to-end
network latency. Tests check result correctness and report timings without a
wall-clock assertion that would fail on noisy CI hosts. Authenticated client integration checks hidden
channels, access revocation, DM membership, and missing authentication.
