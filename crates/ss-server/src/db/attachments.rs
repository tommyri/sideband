//! `attachments` rows.

use sqlx::SqliteConnection;
use ss_core::{Id, Timestamp};

use super::{
    Error,
    convert::{changed, hash32, stamp},
    db,
    model::Attachment,
};

/// Fields stored on insert. The repository sets `created_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAttachment {
    /// Primary key.
    pub id: Id,
    /// Uploader.
    pub uploader_id: Id,
    /// Message, if the file is already attached.
    pub message_id: Option<Id>,
    /// File name.
    pub filename: String,
    /// Media type.
    pub mime: String,
    /// Size in bytes.
    pub size: i64,
    /// SHA-256 of the bytes.
    pub sha256: [u8; 32],
    /// Image width.
    pub width: Option<i64>,
    /// Image height.
    pub height: Option<i64>,
    /// Path relative to the data directory.
    pub storage_path: String,
}

const BY_ID: &str = "SELECT id, uploader_id, message_id, filename, mime, size, sha256, width, height, storage_path, created_at FROM attachments WHERE id = ?";
const BY_MESSAGE: &str = "SELECT id, uploader_id, message_id, filename, mime, size, sha256, width, height, storage_path, created_at FROM attachments WHERE message_id = ? ORDER BY id";

/// Inserts an attachment.
pub async fn insert(
    conn: &mut SqliteConnection,
    attachment: NewAttachment,
) -> Result<Attachment, Error> {
    let created_at = Timestamp::now().as_millis();
    db(sqlx::query(
        "INSERT INTO attachments
         (id, uploader_id, message_id, filename, mime, size, sha256, width, height, storage_path, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(attachment.id)
    .bind(attachment.uploader_id)
    .bind(attachment.message_id)
    .bind(&attachment.filename)
    .bind(&attachment.mime)
    .bind(attachment.size)
    .bind(attachment.sha256.as_slice())
    .bind(attachment.width)
    .bind(attachment.height)
    .bind(&attachment.storage_path)
    .bind(created_at)
    .execute(&mut *conn)
    .await)?;
    get(conn, attachment.id).await?.ok_or(Error::NotFound)
}

/// Loads an attachment by id.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<Attachment>, Error> {
    let row = db(sqlx::query_as::<_, AttachmentRow>(BY_ID)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await)?;
    row.map(AttachmentRow::into_attachment).transpose()
}

/// Attaches the file to a message, or clears the link when `message_id` is `None`.
pub async fn set_message(
    conn: &mut SqliteConnection,
    id: Id,
    message_id: Option<Id>,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE attachments SET message_id = ? WHERE id = ?",
    )
    .bind(message_id)
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Attachments on a message, oldest id first.
pub async fn list_for_message(
    conn: &mut SqliteConnection,
    message_id: Id,
) -> Result<Vec<Attachment>, Error> {
    let rows = db(sqlx::query_as::<_, AttachmentRow>(BY_MESSAGE)
        .bind(message_id)
        .fetch_all(&mut *conn)
        .await)?;
    rows.into_iter()
        .map(AttachmentRow::into_attachment)
        .collect()
}

/// Deletes an attachment.
pub async fn delete(conn: &mut SqliteConnection, id: Id) -> Result<(), Error> {
    changed(db(sqlx::query("DELETE FROM attachments WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await)?)
}

/// Storage path of an existing blob with this hash, if one is already stored.
pub async fn storage_for_hash(
    conn: &mut SqliteConnection,
    sha256: &[u8],
) -> Result<Option<String>, Error> {
    db(sqlx::query_scalar(
        "SELECT storage_path FROM attachments WHERE sha256 = ? ORDER BY id LIMIT 1",
    )
    .bind(sha256)
    .fetch_optional(&mut *conn)
    .await)
}

/// Bytes this user has stored since `since` (Unix milliseconds), including deduped rows.
pub async fn bytes_since(conn: &mut SqliteConnection, user: Id, since: i64) -> Result<i64, Error> {
    db(sqlx::query_scalar(
        "SELECT COALESCE(SUM(size), 0) FROM attachments WHERE uploader_id = ? AND created_at >= ?",
    )
    .bind(user)
    .bind(since)
    .fetch_one(&mut *conn)
    .await)
}

/// Rows that still point at `storage_path`.
pub async fn path_count(conn: &mut SqliteConnection, storage_path: &str) -> Result<i64, Error> {
    db(
        sqlx::query_scalar("SELECT COUNT(*) FROM attachments WHERE storage_path = ?")
            .bind(storage_path)
            .fetch_one(&mut *conn)
            .await,
    )
}

/// Uploads with no message that are older than `before` and are not avatars.
pub async fn unlinked_before(
    conn: &mut SqliteConnection,
    before: i64,
) -> Result<Vec<Attachment>, Error> {
    let rows = db(sqlx::query_as::<_, AttachmentRow>(
        "SELECT id, uploader_id, message_id, filename, mime, size, sha256, width, height, storage_path, created_at
         FROM attachments
         WHERE message_id IS NULL
           AND created_at < ?
           AND id NOT IN (
               SELECT avatar_attachment_id FROM users WHERE avatar_attachment_id IS NOT NULL
           )
         ORDER BY created_at, id",
    )
    .bind(before)
    .fetch_all(&mut *conn)
    .await)?;
    rows.into_iter()
        .map(AttachmentRow::into_attachment)
        .collect()
}

#[derive(Debug, sqlx::FromRow)]
struct AttachmentRow {
    id: Id,
    uploader_id: Id,
    message_id: Option<Id>,
    filename: String,
    mime: String,
    size: i64,
    sha256: Vec<u8>,
    width: Option<i64>,
    height: Option<i64>,
    storage_path: String,
    created_at: i64,
}

impl AttachmentRow {
    fn into_attachment(self) -> Result<Attachment, Error> {
        Ok(Attachment {
            id: self.id,
            uploader_id: self.uploader_id,
            message_id: self.message_id,
            filename: self.filename,
            mime: self.mime,
            size: self.size,
            sha256: hash32(self.sha256)?,
            width: self.width,
            height: self.height,
            storage_path: self.storage_path,
            created_at: stamp(self.created_at),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{NewAttachment, delete, get, insert, list_for_message, set_message};
    use crate::db::testing;

    #[tokio::test]
    async fn insert_link_and_delete() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let user = testing::user(&mut conn, 1).await;
        let channel = testing::text_channel(&mut conn, 2, user.id).await;
        let message = testing::message(&mut conn, 3, channel.id, user.id).await;
        let sha256 = [7_u8; 32];
        let created = insert(
            &mut conn,
            NewAttachment {
                id: testing::id(4),
                uploader_id: user.id,
                message_id: None,
                filename: "note.txt".into(),
                mime: "text/plain".into(),
                size: 4,
                sha256,
                width: None,
                height: None,
                storage_path: "files/note.txt".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(created.sha256, sha256);
        set_message(&mut conn, created.id, Some(message.id))
            .await
            .unwrap();
        assert_eq!(
            list_for_message(&mut conn, message.id).await.unwrap()[0].id,
            created.id
        );
        delete(&mut conn, created.id).await.unwrap();
        assert!(get(&mut conn, created.id).await.unwrap().is_none());
    }
}
