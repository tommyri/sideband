//! `link_previews` and `message_links` rows.
//!
//! The primary key is the SHA-256 of the URL string. Callers pass the URL;
//! this module hashes it.

use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
use ss_core::{Id, Timestamp};

use super::{
    Error,
    convert::{changed, hash32, stamp},
    db,
    model::{LinkPreview, MessageLink, PreviewStatus},
};

/// Fields stored for one URL. The repository hashes the URL and sets `fetched_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLinkPreview {
    /// URL text.
    pub url: String,
    /// Fetch state.
    pub status: PreviewStatus,
    /// Page title.
    pub title: Option<String>,
    /// Page description.
    pub description: Option<String>,
    /// Preview image URL.
    pub image_url: Option<String>,
    /// Site name.
    pub site_name: Option<String>,
    /// When the cache should be refreshed.
    pub expires_at: Timestamp,
}

/// SHA-256 of a URL string.
#[must_use]
pub fn url_hash(url: &str) -> [u8; 32] {
    Sha256::digest(url.as_bytes()).into()
}

const BY_HASH: &str = "SELECT url_hash, url, status, title, description, image_url, site_name, fetched_at, expires_at FROM link_previews WHERE url_hash = ?";

/// Inserts or replaces the preview for `preview.url`.
pub async fn upsert(
    conn: &mut SqliteConnection,
    preview: NewLinkPreview,
) -> Result<LinkPreview, Error> {
    let hash = url_hash(&preview.url);
    let fetched_at = Timestamp::now().as_millis();
    db(sqlx::query(
        "INSERT INTO link_previews
         (url_hash, url, status, title, description, image_url, site_name, fetched_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (url_hash) DO UPDATE SET
            url = excluded.url,
            status = excluded.status,
            title = excluded.title,
            description = excluded.description,
            image_url = excluded.image_url,
            site_name = excluded.site_name,
            fetched_at = excluded.fetched_at,
            expires_at = excluded.expires_at",
    )
    .bind(hash.as_slice())
    .bind(&preview.url)
    .bind(preview.status as i64)
    .bind(&preview.title)
    .bind(&preview.description)
    .bind(&preview.image_url)
    .bind(&preview.site_name)
    .bind(fetched_at)
    .bind(preview.expires_at.as_millis())
    .execute(&mut *conn)
    .await)?;
    get(conn, &preview.url).await?.ok_or(Error::NotFound)
}

/// Loads the preview for a URL.
pub async fn get(conn: &mut SqliteConnection, url: &str) -> Result<Option<LinkPreview>, Error> {
    let row = db(sqlx::query_as::<_, PreviewRow>(BY_HASH)
        .bind(url_hash(url).as_slice())
        .fetch_optional(&mut *conn)
        .await)?;
    row.map(PreviewRow::into_preview).transpose()
}

/// Deletes a preview. Linked messages must be unlinked first.
pub async fn delete(conn: &mut SqliteConnection, url: &str) -> Result<(), Error> {
    changed(db(sqlx::query(
        "DELETE FROM link_previews WHERE url_hash = ?",
    )
    .bind(url_hash(url).as_slice())
    .execute(&mut *conn)
    .await)?)
}

/// Attaches a preview to a message. The preview row must already exist.
pub async fn link(
    conn: &mut SqliteConnection,
    message_id: Id,
    url: &str,
    position: i64,
) -> Result<(), Error> {
    db(sqlx::query(
        "INSERT INTO message_links (message_id, url_hash, position) VALUES (?, ?, ?)
         ON CONFLICT (message_id, url_hash) DO UPDATE SET position = excluded.position",
    )
    .bind(message_id)
    .bind(url_hash(url).as_slice())
    .bind(position)
    .execute(&mut *conn)
    .await)?;
    Ok(())
}

/// Removes one link.
pub async fn unlink(conn: &mut SqliteConnection, message_id: Id, url: &str) -> Result<(), Error> {
    changed(db(sqlx::query(
        "DELETE FROM message_links WHERE message_id = ? AND url_hash = ?",
    )
    .bind(message_id)
    .bind(url_hash(url).as_slice())
    .execute(&mut *conn)
    .await)?)
}

/// Links on a message, in position order.
pub async fn links(conn: &mut SqliteConnection, message_id: Id) -> Result<Vec<MessageLink>, Error> {
    let rows = db(sqlx::query_as::<_, LinkRow>(
        "SELECT message_id, url_hash, position FROM message_links WHERE message_id = ? ORDER BY position, url_hash",
    )
    .bind(message_id)
    .fetch_all(&mut *conn)
    .await)?;
    rows.into_iter().map(LinkRow::into_link).collect()
}

#[derive(Debug, sqlx::FromRow)]
struct PreviewRow {
    url_hash: Vec<u8>,
    url: String,
    status: i64,
    title: Option<String>,
    description: Option<String>,
    image_url: Option<String>,
    site_name: Option<String>,
    fetched_at: i64,
    expires_at: i64,
}

impl PreviewRow {
    fn into_preview(self) -> Result<LinkPreview, Error> {
        Ok(LinkPreview {
            url_hash: hash32(self.url_hash)?,
            url: self.url,
            status: PreviewStatus::from_i64(self.status)?,
            title: self.title,
            description: self.description,
            image_url: self.image_url,
            site_name: self.site_name,
            fetched_at: stamp(self.fetched_at),
            expires_at: stamp(self.expires_at),
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct LinkRow {
    message_id: Id,
    url_hash: Vec<u8>,
    position: i64,
}

impl LinkRow {
    fn into_link(self) -> Result<MessageLink, Error> {
        Ok(MessageLink {
            message_id: self.message_id,
            url_hash: hash32(self.url_hash)?,
            position: self.position,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{NewLinkPreview, delete, get, link, links, unlink, upsert};
    use crate::db::{model::PreviewStatus, testing};

    #[tokio::test]
    async fn upsert_link_and_delete() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let user = testing::user(&mut conn, 1).await;
        let channel = testing::text_channel(&mut conn, 2, user.id).await;
        let message = testing::message(&mut conn, 3, channel.id, user.id).await;
        let url = "https://example.com/a";
        let created = upsert(
            &mut conn,
            NewLinkPreview {
                url: url.into(),
                status: PreviewStatus::Ready,
                title: Some("Example".into()),
                description: None,
                image_url: None,
                site_name: Some("Example".into()),
                expires_at: ss_core::Timestamp::from_millis(100),
            },
        )
        .await
        .unwrap();
        assert_eq!(created.url_hash, super::url_hash(url));
        let updated = upsert(
            &mut conn,
            NewLinkPreview {
                url: url.into(),
                status: PreviewStatus::Failed,
                title: None,
                description: None,
                image_url: None,
                site_name: None,
                expires_at: ss_core::Timestamp::from_millis(200),
            },
        )
        .await
        .unwrap();
        assert_eq!(updated.status, PreviewStatus::Failed);
        link(&mut conn, message.id, url, 0).await.unwrap();
        link(&mut conn, message.id, url, 2).await.unwrap();
        assert_eq!(links(&mut conn, message.id).await.unwrap()[0].position, 2);
        unlink(&mut conn, message.id, url).await.unwrap();
        delete(&mut conn, url).await.unwrap();
        assert!(get(&mut conn, url).await.unwrap().is_none());
    }
}
