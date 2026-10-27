-- Schema from docs/plan/03-data-model.md.
-- Ids are 16-byte ULIDs. Timestamps are Unix milliseconds.
-- Seeded role ids match `db::roles::EVERYONE_ROLE_ID` and `ADMIN_ROLE_ID`.

CREATE TABLE roles (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    name TEXT NOT NULL,
    color INTEGER,
    permissions INTEGER NOT NULL,
    position INTEGER NOT NULL,
    is_default INTEGER NOT NULL CHECK (is_default IN (0, 1)),
    created_at INTEGER NOT NULL
);

CREATE TABLE users (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    username TEXT NOT NULL COLLATE NOCASE UNIQUE,
    display_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    avatar_attachment_id BLOB CHECK (
        avatar_attachment_id IS NULL OR length(avatar_attachment_id) = 16
    ) REFERENCES attachments (id),
    kind INTEGER NOT NULL CHECK (kind IN (0, 1)),
    created_at INTEGER NOT NULL,
    disabled_at INTEGER
);

CREATE TABLE user_roles (
    user_id BLOB NOT NULL CHECK (length(user_id) = 16) REFERENCES users (id),
    role_id BLOB NOT NULL CHECK (length(role_id) = 16) REFERENCES roles (id),
    PRIMARY KEY (user_id, role_id)
);

CREATE TABLE sessions (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    user_id BLOB NOT NULL CHECK (length(user_id) = 16) REFERENCES users (id),
    token_hash TEXT NOT NULL UNIQUE,
    device_name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    expires_at INTEGER
);

CREATE TABLE invites (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    code TEXT NOT NULL UNIQUE,
    created_by BLOB NOT NULL CHECK (length(created_by) = 16) REFERENCES users (id),
    max_uses INTEGER,
    uses INTEGER NOT NULL,
    expires_at INTEGER,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL
);

CREATE TABLE channels (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    kind INTEGER NOT NULL CHECK (kind IN (0, 1, 2, 3, 4)),
    name TEXT NOT NULL,
    topic TEXT,
    parent_id BLOB CHECK (parent_id IS NULL OR length(parent_id) = 16) REFERENCES channels (id),
    position INTEGER NOT NULL,
    voice_profile TEXT CHECK (
        voice_profile IS NULL OR voice_profile IN ('voice', 'music', 'low_latency')
    ),
    voice_bitrate INTEGER,
    user_limit INTEGER,
    created_by BLOB NOT NULL CHECK (length(created_by) = 16) REFERENCES users (id),
    created_at INTEGER NOT NULL,
    deleted_at INTEGER
);

CREATE TABLE channel_members (
    channel_id BLOB NOT NULL CHECK (length(channel_id) = 16) REFERENCES channels (id),
    user_id BLOB NOT NULL CHECK (length(user_id) = 16) REFERENCES users (id),
    PRIMARY KEY (channel_id, user_id)
);

CREATE TABLE channel_overrides (
    channel_id BLOB NOT NULL CHECK (length(channel_id) = 16) REFERENCES channels (id),
    role_id BLOB NOT NULL CHECK (length(role_id) = 16) REFERENCES roles (id),
    allow INTEGER NOT NULL,
    deny INTEGER NOT NULL,
    PRIMARY KEY (channel_id, role_id)
);

CREATE TABLE webhooks (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    channel_id BLOB NOT NULL CHECK (length(channel_id) = 16) REFERENCES channels (id),
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_by BLOB NOT NULL CHECK (length(created_by) = 16) REFERENCES users (id),
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);

CREATE TABLE messages (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    channel_id BLOB NOT NULL CHECK (length(channel_id) = 16) REFERENCES channels (id),
    author_id BLOB CHECK (author_id IS NULL OR length(author_id) = 16) REFERENCES users (id),
    webhook_id BLOB CHECK (webhook_id IS NULL OR length(webhook_id) = 16) REFERENCES webhooks (id),
    content TEXT NOT NULL,
    reply_to BLOB CHECK (reply_to IS NULL OR length(reply_to) = 16) REFERENCES messages (id),
    created_at INTEGER NOT NULL,
    edited_at INTEGER,
    deleted_at INTEGER
);

CREATE TABLE message_mentions (
    message_id BLOB NOT NULL CHECK (length(message_id) = 16) REFERENCES messages (id),
    user_id BLOB NOT NULL CHECK (length(user_id) = 16) REFERENCES users (id),
    PRIMARY KEY (message_id, user_id)
);

CREATE TABLE attachments (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    uploader_id BLOB NOT NULL CHECK (length(uploader_id) = 16) REFERENCES users (id),
    message_id BLOB CHECK (message_id IS NULL OR length(message_id) = 16) REFERENCES messages (id),
    filename TEXT NOT NULL,
    mime TEXT NOT NULL,
    size INTEGER NOT NULL,
    sha256 BLOB NOT NULL CHECK (length(sha256) = 32),
    width INTEGER,
    height INTEGER,
    storage_path TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE link_previews (
    url_hash BLOB PRIMARY KEY NOT NULL CHECK (length(url_hash) = 32),
    url TEXT NOT NULL,
    status INTEGER NOT NULL CHECK (status IN (0, 1, 2)),
    title TEXT,
    description TEXT,
    image_url TEXT,
    site_name TEXT,
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE message_links (
    message_id BLOB NOT NULL CHECK (length(message_id) = 16) REFERENCES messages (id),
    url_hash BLOB NOT NULL CHECK (length(url_hash) = 32) REFERENCES link_previews (url_hash),
    position INTEGER NOT NULL,
    PRIMARY KEY (message_id, url_hash)
);

CREATE TABLE read_states (
    user_id BLOB NOT NULL CHECK (length(user_id) = 16) REFERENCES users (id),
    channel_id BLOB NOT NULL CHECK (length(channel_id) = 16) REFERENCES channels (id),
    last_read_message_id BLOB NOT NULL CHECK (length(last_read_message_id) = 16) REFERENCES messages (id),
    PRIMARY KEY (user_id, channel_id)
);

CREATE TABLE bans (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    user_id BLOB NOT NULL CHECK (length(user_id) = 16) REFERENCES users (id),
    banned_by BLOB NOT NULL CHECK (length(banned_by) = 16) REFERENCES users (id),
    reason TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    lifted_at INTEGER
);

CREATE TABLE audit_log (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    actor_id BLOB CHECK (actor_id IS NULL OR length(actor_id) = 16) REFERENCES users (id),
    action TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id BLOB CHECK (target_id IS NULL OR length(target_id) = 16),
    details TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE server_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

CREATE INDEX messages_channel_id_id ON messages (channel_id, id DESC);
CREATE INDEX messages_author_id ON messages (author_id);
CREATE INDEX attachments_message_id ON attachments (message_id);
CREATE INDEX sessions_user_id ON sessions (user_id);
CREATE INDEX bans_user_id_lifted_at ON bans (user_id, lifted_at);
CREATE INDEX audit_log_created_at ON audit_log (created_at);
CREATE INDEX link_previews_expires_at ON link_previews (expires_at);

-- Permissions::EVERYONE is 388608. Permissions::ADMINISTRATOR is 1.
-- Admin sits one position above @everyone. Higher position outranks.
INSERT INTO roles (id, name, color, permissions, position, is_default, created_at)
VALUES
    (X'00000000000000000000000000000001', '@everyone', NULL, 388608, 0, 1, 0),
    (X'00000000000000000000000000000002', 'Admin', NULL, 1, 1, 0, 0);

INSERT INTO server_settings (key, value)
VALUES
    ('name', 'SocialSpeak'),
    ('description', ''),
    ('icon', ''),
    ('registration_mode', 'invite');
