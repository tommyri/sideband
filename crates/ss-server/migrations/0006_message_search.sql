-- Index live text only. The view keeps soft-deleted rows out of rebuilds.
CREATE VIEW searchable_messages AS
SELECT rowid, content FROM messages WHERE deleted_at IS NULL;
CREATE VIRTUAL TABLE message_search USING fts5(
    content, content='searchable_messages', content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER message_search_insert AFTER INSERT ON messages WHEN new.deleted_at IS NULL BEGIN
    INSERT INTO message_search(rowid, content) VALUES(new.rowid, new.content);
END;
CREATE TRIGGER message_search_delete AFTER DELETE ON messages WHEN old.deleted_at IS NULL BEGIN
    INSERT INTO message_search(message_search, rowid, content) VALUES('delete', old.rowid, old.content);
END;
CREATE TRIGGER message_search_update AFTER UPDATE OF content, deleted_at ON messages BEGIN
    INSERT INTO message_search(message_search, rowid, content)
    SELECT 'delete', old.rowid, old.content WHERE old.deleted_at IS NULL;
    INSERT INTO message_search(rowid, content)
    SELECT new.rowid, new.content WHERE new.deleted_at IS NULL;
END;
INSERT INTO message_search(message_search) VALUES('rebuild');
