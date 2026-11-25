-- One bounded encrypted copy per local member. No credential or plaintext payload.
CREATE TABLE profile_sync_copies (
    user_id BLOB PRIMARY KEY NOT NULL REFERENCES profile_memberships(user_id) ON DELETE CASCADE,
    ciphertext BLOB NOT NULL CHECK (length(ciphertext) BETWEEN 29 AND 524288)
);
