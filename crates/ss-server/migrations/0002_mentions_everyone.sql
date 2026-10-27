-- Authorized @everyone pings. The raw text can contain the word without this bit.
ALTER TABLE messages ADD COLUMN mentions_everyone INTEGER NOT NULL DEFAULT 0;
