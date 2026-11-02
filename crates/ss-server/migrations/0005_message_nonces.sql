CREATE TABLE message_nonces (
    user_id BLOB NOT NULL CHECK (length(user_id) = 16) REFERENCES users (id),
    nonce TEXT NOT NULL,
    message_id BLOB NOT NULL CHECK (length(message_id) = 16) REFERENCES messages (id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, nonce)
);

CREATE INDEX message_nonces_created_at ON message_nonces (created_at);
