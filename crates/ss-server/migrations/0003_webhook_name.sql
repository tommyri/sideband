-- Name shown for a webhook post. User messages leave this null.
ALTER TABLE messages ADD COLUMN webhook_name TEXT;
