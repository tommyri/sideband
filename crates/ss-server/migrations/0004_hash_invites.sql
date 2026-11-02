INSERT INTO server_settings (key, value)
SELECT 'migration.invites_plaintext_invalidated', CAST(COUNT(*) AS TEXT)
FROM invites
HAVING COUNT(*) > 0;

ALTER TABLE invites RENAME TO invites_plaintext;

CREATE TABLE invites (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    code_hash TEXT NOT NULL UNIQUE CHECK (length(code_hash) = 64),
    created_by BLOB NOT NULL CHECK (length(created_by) = 16) REFERENCES users (id),
    max_uses INTEGER,
    uses INTEGER NOT NULL,
    expires_at INTEGER,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL
);

-- Existing codes cannot be hashed safely inside SQLite. Preserve the audit
-- metadata, replace each code with an unguessable unusable digest, and revoke it.
INSERT INTO invites (
    id, code_hash, created_by, max_uses, uses, expires_at, revoked_at, created_at
)
SELECT
    id,
    lower(hex(randomblob(32))),
    created_by,
    max_uses,
    uses,
    expires_at,
    COALESCE(revoked_at, unixepoch('subsec') * 1000),
    created_at
FROM invites_plaintext;

DROP TABLE invites_plaintext;
