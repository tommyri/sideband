# Data model

SQLite, WAL mode, `sqlx` with embedded migrations in
`crates/ss-server/migrations/`. All ids are ULIDs stored as 16-byte BLOBs
(`ss_core::Id`). Timestamps are `INTEGER` Unix milliseconds. Soft deletes only
where history matters (messages).

## Tables

```sql
users            (id PK, username UNIQUE COLLATE NOCASE, display_name, password_hash,
                  avatar_attachment_id NULL, kind INTEGER /*0 user,1 bot*/, created_at,
                  disabled_at NULL)
sessions         (id PK, user_id FK, token_hash UNIQUE, device_name, created_at,
                  last_seen_at, expires_at NULL)
invites          (id PK, code UNIQUE, created_by FK users, max_uses NULL, uses,
                  expires_at NULL, revoked_at NULL, created_at)

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
                  deleted_at NULL)
message_mentions (message_id FK, user_id FK, PRIMARY KEY(message_id,user_id))
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
server_settings  (key PK, value TEXT)  -- name, description, icon, registration_mode
```

Indexes: `messages(channel_id, id DESC)`, `messages(author_id)`,
`attachments(message_id)`, `sessions(user_id)`, `bans(user_id, lifted_at)`,
`audit_log(created_at)`, `link_previews(expires_at)`.

Full-text search over `messages.content` via FTS5 external-content table is a
phase 4 task.

## Permissions

`ss_core::Permissions` is a `bitflags! u64`:

| Bit | Name | Grants |
|---|---|---|
| 0 | `ADMINISTRATOR` | Everything; bypasses overrides. Only owner-created roles can carry it |
| 1 | `MANAGE_SERVER` | Server settings, invites, webhooks |
| 2 | `MANAGE_CHANNELS` | Create/update/delete/move channels, overrides |
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
| 18 | `CHANGE_NICKNAME` | |

Default `@everyone` role: `VIEW_CHANNEL | SEND_MESSAGES | ATTACH_FILES |
CONNECT | SPEAK | STREAM | CREATE_INVITES | CHANGE_NICKNAME`. Default `Admin`
role: `ADMINISTRATOR`. The owner (first user) gets `Admin`.

Resolution for user `u` in channel `c`:

```
base   = OR(permissions of u's roles) | @everyone.permissions
if base has ADMINISTRATOR → allow all
for each role r of u (plus @everyone), override = channel_overrides[c][r]:
    base = (base & ~override.deny) | override.allow
DM channels: VIEW_CHANNEL|SEND_MESSAGES|ATTACH_FILES iff u ∈ channel_members
```

Hierarchy rule: an actor may kick/ban/mute/move/edit-roles-of a target only if
`actor.highest_role.position > target.highest_role.position` (owner always
wins). Roles can only be granted/edited if positioned below the actor's highest
role.

Every command handler calls `perms.require(user, channel?, Permissions::X)`.
A table-driven test in `ss-server/tests/permissions.rs` covers every command.

## Ids and ordering

ULIDs give time-ordered ids, so `messages.id` doubles as a cursor for
pagination (`before=<id>`), and clients can sort without timestamps.
