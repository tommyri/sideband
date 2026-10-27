//! `webhooks` rows.

use sqlx::SqliteConnection;
use ss_core::{Id, Timestamp};

use super::{
    Error,
    convert::{changed, stamp, stamp_opt},
    db,
    model::Webhook,
};

/// Fields stored on insert. The repository sets `created_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWebhook {
    /// Primary key.
    pub id: Id,
    /// Channel the webhook posts into.
    pub channel_id: Id,
    /// Display name.
    pub name: String,
    /// Hash of the token.
    pub token_hash: String,
    /// Creator.
    pub created_by: Id,
}

const BY_ID: &str = "SELECT id, channel_id, name, token_hash, created_by, created_at, revoked_at FROM webhooks WHERE id = ?";
const BY_TOKEN: &str = "SELECT id, channel_id, name, token_hash, created_by, created_at, revoked_at FROM webhooks WHERE token_hash = ?";

/// Inserts a webhook.
pub async fn insert(conn: &mut SqliteConnection, webhook: NewWebhook) -> Result<Webhook, Error> {
    let created_at = Timestamp::now().as_millis();
    db(sqlx::query(
        "INSERT INTO webhooks (id, channel_id, name, token_hash, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(webhook.id)
    .bind(webhook.channel_id)
    .bind(&webhook.name)
    .bind(&webhook.token_hash)
    .bind(webhook.created_by)
    .bind(created_at)
    .execute(&mut *conn)
    .await)?;
    get(conn, webhook.id).await?.ok_or(Error::NotFound)
}

/// Loads a webhook by id.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<Webhook>, Error> {
    let row = db(sqlx::query_as::<_, WebhookRow>(BY_ID)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await)?;
    Ok(row.map(WebhookRow::into_webhook))
}

/// Loads a webhook by token hash.
pub async fn get_by_token_hash(
    conn: &mut SqliteConnection,
    token_hash: &str,
) -> Result<Option<Webhook>, Error> {
    let row = db(sqlx::query_as::<_, WebhookRow>(BY_TOKEN)
        .bind(token_hash)
        .fetch_optional(&mut *conn)
        .await)?;
    Ok(row.map(WebhookRow::into_webhook))
}

/// Sets `revoked_at`.
pub async fn revoke(
    conn: &mut SqliteConnection,
    id: Id,
    revoked_at: Timestamp,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE webhooks SET revoked_at = ? WHERE id = ?",
    )
    .bind(revoked_at.as_millis())
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Deletes a webhook.
pub async fn delete(conn: &mut SqliteConnection, id: Id) -> Result<(), Error> {
    changed(db(sqlx::query("DELETE FROM webhooks WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await)?)
}

#[derive(Debug, sqlx::FromRow)]
struct WebhookRow {
    id: Id,
    channel_id: Id,
    name: String,
    token_hash: String,
    created_by: Id,
    created_at: i64,
    revoked_at: Option<i64>,
}

impl WebhookRow {
    fn into_webhook(self) -> Webhook {
        Webhook {
            id: self.id,
            channel_id: self.channel_id,
            name: self.name,
            token_hash: self.token_hash,
            created_by: self.created_by,
            created_at: stamp(self.created_at),
            revoked_at: stamp_opt(self.revoked_at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NewWebhook, delete, get_by_token_hash, insert, revoke};
    use crate::db::testing;

    #[tokio::test]
    async fn insert_revoke_and_delete() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let user = testing::user(&mut conn, 1).await;
        let channel = testing::text_channel(&mut conn, 2, user.id).await;
        let created = insert(
            &mut conn,
            NewWebhook {
                id: testing::id(3),
                channel_id: channel.id,
                name: "builds".into(),
                token_hash: "secret".into(),
                created_by: user.id,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            get_by_token_hash(&mut conn, "secret")
                .await
                .unwrap()
                .unwrap()
                .id,
            created.id
        );
        let revoked_at = ss_core::Timestamp::from_millis(6);
        revoke(&mut conn, created.id, revoked_at).await.unwrap();
        assert_eq!(
            get_by_token_hash(&mut conn, "secret")
                .await
                .unwrap()
                .unwrap()
                .revoked_at,
            Some(revoked_at)
        );
        delete(&mut conn, created.id).await.unwrap();
        assert!(
            get_by_token_hash(&mut conn, "secret")
                .await
                .unwrap()
                .is_none()
        );
    }
}
