-- Bot tokens (T-080). A bot is a `users` row with kind 1; it has no password
-- and no sessions. Only the SHA-256 hex digest of a token is stored. A bot
-- has at most one live token (`revoked_at` NULL); `scopes` caps the bits its
-- roles grant while that token is used. `created_by` is NULL when the
-- operator CLI issued it.
CREATE TABLE bot_tokens (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    bot_id BLOB NOT NULL CHECK (length(bot_id) = 16) REFERENCES users (id),
    token_hash TEXT NOT NULL UNIQUE,
    scopes INTEGER NOT NULL CHECK (scopes >= 0),
    created_by BLOB CHECK (created_by IS NULL OR length(created_by) = 16) REFERENCES users (id),
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);

CREATE UNIQUE INDEX bot_tokens_live ON bot_tokens (bot_id) WHERE revoked_at IS NULL;
