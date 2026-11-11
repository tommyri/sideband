-- DM lookups by member (the DM page, the group-DM join count) search this
-- instead of scanning every membership. Covering, so they skip the table.
CREATE INDEX channel_members_user_id ON channel_members (user_id, channel_id);
