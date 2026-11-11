# Data model

SQLite, WAL mode, `sqlx` with embedded migrations in
`crates/ss-server/migrations/` (0001 to 0006; the listing below is the schema
after all of them). All ids are ULIDs stored as 16-byte BLOBs
(`ss_core::Id`). Timestamps are `INTEGER` Unix milliseconds. Soft deletes only
where history matters (messages).

## Tables

```sql
users            (id PK, username UNIQUE COLLATE NOCASE, display_name, password_hash,
                  avatar_attachment_id NULL, kind INTEGER /*0 user,1 bot*/, created_at,
                  disabled_at NULL)
sessions         (id PK, user_id FK, token_hash UNIQUE, device_name, created_at,
                  last_seen_at, expires_at NULL)
invites          (id PK, code_hash UNIQUE /*0004: SHA-256 hex*/, created_by FK users,
                  max_uses NULL, uses, expires_at NULL, revoked_at NULL, created_at)

roles            (id PK, name, color NULL, permissions INTEGER /*u64 bitflags*/,
                  position INTEGER, is_default INTEGER /*the @everyone role*/,
                  created_at)
user_roles       (user_id FK, role_id FK, PRIMARY KEY(user_id, role_id))

channels         (id PK, kind INTEGER /*0 category,1 text,2 voice,3 dm,4 group_dm*/,
                  name, topic NULL, parent_id FK channels NULL, position INTEGER,
                  voice_profile TEXT NULL /*voice|music|low_latency*/,
                  voice_bitrate INTEGER NULL, user_limit INTEGER NULL,
                  created_by FK users, created_at, deleted_at NULL)
channel_members  (channel_id FK, user_id FK, PRIMARY KEY(channel_id,user_id))  -- DMs only
channel_overrides(channel_id FK, role_id FK, allow INTEGER, deny INTEGER,
                  PRIMARY KEY(channel_id, role_id))

messages         (id PK, channel_id FK, author_id FK users NULL, webhook_id FK NULL,
                  content TEXT, reply_to FK messages NULL, created_at, edited_at NULL,
                  deleted_at NULL,
                  mentions_everyone INTEGER DEFAULT 0 /*0002: @everyone in content*/,
                  webhook_name NULL /*0003: name shown for a webhook post*/)
message_mentions (message_id FK, user_id FK, PRIMARY KEY(message_id,user_id))
message_nonces   (user_id FK, nonce TEXT, message_id FK, created_at,
                  PRIMARY KEY(user_id, nonce))  -- 0005: SendMessage replay dedupe
message_search   FTS5(content) over view searchable_messages (rowid, content of
                  messages not deleted), kept by triggers  -- 0006
attachments      (id PK, uploader_id FK, message_id FK NULL, filename, mime, size,
                  sha256, width NULL, height NULL, storage_path, created_at)
link_previews    (url_hash PK, url, status INTEGER, title NULL, description NULL,
                  image_url NULL, site_name NULL, fetched_at, expires_at)
message_links    (message_id FK, url_hash FK, position INTEGER,
                  PRIMARY KEY(message_id, url_hash))
read_states      (user_id FK, channel_id FK, last_read_message_id,
                  PRIMARY KEY(user_id, channel_id))

bans             (id PK, user_id FK, banned_by FK users, reason NULL,
                  created_at, expires_at NULL, lifted_at NULL)
audit_log        (id PK, actor_id FK NULL, action TEXT, target_kind TEXT,
                  target_id BLOB NULL, details TEXT /*json*/, created_at)
webhooks         (id PK, channel_id FK, name, token_hash UNIQUE, created_by FK,
                  created_at, revoked_at NULL)
server_settings  (key PK, value TEXT)  -- name, description, icon, registration_mode,
                                         -- owner_invite until the first account
```

Indexes: `messages(channel_id, id DESC)`, `messages(author_id)`,
`attachments(message_id)`, `sessions(user_id)`, `bans(user_id, lifted_at)`,
`audit_log(created_at)`, `link_previews(expires_at)`,
`message_nonces(created_at)`.

Nonces are kept 5 minutes after the message was stored (`02-protocol.md`
§1.2). Message search (T-065, migration 0006) is an FTS5 external-content
table with the `unicode61 remove_diacritics 2` tokenizer; deleting or
soft-deleting a message removes it from the index, and `ss-server
reindex-search` rebuilds it.

## Permissions

`ss_core::Permissions` is a `bitflags! u64`:

| Bit | Name | Grants |
|---|---|---|
| 0 | `ADMINISTRATOR` | Everything; bypasses overrides. Only an administrator can grant it |
| 1 | `MANAGE_SERVER` | Server settings, invites, webhooks |
| 2 | `MANAGE_CHANNELS` | Create/update/delete/move channels they can see, overrides on them |
| 3 | `MANAGE_ROLES` | Create/update/delete roles below own highest role; assign them |
| 4 | `MANAGE_MESSAGES` | Delete others' messages, pin |
| 5 | `KICK_MEMBERS` | |
| 6 | `BAN_MEMBERS` | |
| 7 | `MUTE_MEMBERS` | Server mute/unmute |
| 8 | `MOVE_MEMBERS` | Move users between voice channels |
| 9 | `VIEW_CHANNEL` | See channel and read history |
| 10 | `SEND_MESSAGES` | |
| 11 | `ATTACH_FILES` | |
| 12 | `MENTION_EVERYONE` | |
| 13 | `CONNECT` | Join voice channel |
| 14 | `SPEAK` | Send audio |
| 15 | `STREAM` | Share screen (phase 5) |
| 16 | `CREATE_INVITES` | |
| 17 | `VIEW_AUDIT_LOG` | |
| 18 | `CHANGE_NICKNAME` | Change own display name |

Default `@everyone` role: `VIEW_CHANNEL | SEND_MESSAGES | ATTACH_FILES |
CONNECT | SPEAK | STREAM | CREATE_INVITES | CHANGE_NICKNAME`. Default `Admin`
role: `ADMINISTRATOR`. The owner (first user) gets `Admin`. The seeded `Admin`
role cannot be deleted and cannot lose `ADMINISTRATOR`, so `role grant-admin`
always has a role to grant.

Resolution for user `u` in channel `c`:

```
base   = OR(permissions of u's roles) | @everyone.permissions
if base has ADMINISTRATOR → allow all
for each role r of u (plus @everyone), override = channel_overrides[c][r]:
    base = (base & ~override.deny) | override.allow
DM channels: VIEW_CHANNEL|SEND_MESSAGES|ATTACH_FILES iff u ∈ channel_members
```

Hierarchy rule: an actor may kick/ban/mute/move/edit-roles-of a target only if
`actor.highest_role.position > target.highest_role.position`. An administrator
may act on anyone, including another administrator, and a non-administrator may
never act on an administrator, whatever the positions. Roles can only be
granted/edited if positioned below the actor's highest role.

There is no owner above the administrators. The first user is an administrator
like any other, so give `ADMINISTRATOR` only to people trusted with the whole
server: one administrator can ban or demote another. The operator's CLI
(`role grant-admin`, `user unban`) is the recovery path. (T-090 chose this over
adding an owner column; the earlier text here promised an owner who always wins,
which the code never had.)

Granting rule: nobody can grant a bit they do not hold. A role create, a role
edit, and a role assignment may only add bits the actor holds on the server.
An override may only add `allow` bits the actor holds in that channel. Bits a
role or override already has may stay, and removing bits is not a grant.
`deny` masks are not limited.

Visibility rule: a command that names a channel needs `VIEW_CHANNEL` on it, and
a channel the actor cannot see answers as not found. This covers overrides,
channel update, move, and delete, a new parent category, and both channels of a
`MoveUser`. A category delete that would remove a channel the actor cannot see
is forbidden.

Voice seats follow permissions. After every command that changes them, a
seated member who lost `VIEW_CHANNEL`, or `CONNECT` on a seat they joined
themselves, is removed and the leave is announced. A seat placed by `MoveUser`
keeps bypassing `CONNECT` but not view. A resume rechecks the leased seat before
restoring it. Banning a member revokes every open invite they created.

Every command handler calls `perms.require(user, channel?, Permissions::X)`.
A table-driven test in `ss-server/tests/permissions.rs` covers every command.
`ss-server/tests/authorization_audit.rs` scans the handler source and fails
when a command or HTTP route is unclassified or its handler stops reaching
its permission check (see `docs/security.md` section 5).

## Ids and ordering

ULIDs give time-ordered ids, so `messages.id` doubles as a cursor for
pagination (`before=<id>`), and clients can sort without timestamps.
