-- The users row remains the authority for roles, bans and message history.
-- These public keys are unique to this server. No profile-wide identifier is stored.
CREATE TABLE profile_memberships (
    membership_key BLOB PRIMARY KEY NOT NULL CHECK (length(membership_key) = 32),
    user_id BLOB UNIQUE NOT NULL REFERENCES users(id) ON DELETE CASCADE
);

CREATE TABLE profile_devices (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    device_key BLOB UNIQUE NOT NULL CHECK (length(device_key) = 32),
    membership_key BLOB NOT NULL REFERENCES profile_memberships(membership_key) ON DELETE CASCADE,
    device_name TEXT NOT NULL,
    revoked_at INTEGER
);
CREATE INDEX profile_devices_membership ON profile_devices(membership_key);

ALTER TABLE sessions ADD COLUMN profile_device_id BLOB REFERENCES profile_devices(id);
CREATE INDEX sessions_profile_device ON sessions(profile_device_id) WHERE profile_device_id IS NOT NULL;
