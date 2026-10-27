//! `bans` rows.

use sqlx::SqliteConnection;
use ss_core::{Id, Timestamp};

use super::{
    Error,
    convert::{changed, stamp, stamp_opt},
    db,
    model::Ban,
};

/// Fields stored on insert. The repository sets `created_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBan {
    /// Primary key.
    pub id: Id,
    /// Banned user.
    pub user_id: Id,
    /// Moderator.
    pub banned_by: Id,
    /// Optional reason.
    pub reason: Option<String>,
    /// Optional expiry.
    pub expires_at: Option<Timestamp>,
}

const BY_ID: &str = "SELECT id, user_id, banned_by, reason, created_at, expires_at, lifted_at FROM bans WHERE id = ?";
const BY_USER: &str = "SELECT id, user_id, banned_by, reason, created_at, expires_at, lifted_at FROM bans WHERE user_id = ? ORDER BY id DESC";

/// Inserts a ban.
pub async fn insert(conn: &mut SqliteConnection, ban: NewBan) -> Result<Ban, Error> {
    let created_at = Timestamp::now().as_millis();
    db(sqlx::query(
        "INSERT INTO bans (id, user_id, banned_by, reason, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(ban.id)
    .bind(ban.user_id)
    .bind(ban.banned_by)
    .bind(&ban.reason)
    .bind(created_at)
    .bind(ban.expires_at.map(Timestamp::as_millis))
    .execute(&mut *conn)
    .await)?;
    get(conn, ban.id).await?.ok_or(Error::NotFound)
}

/// Loads a ban by id.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<Ban>, Error> {
    let row = db(sqlx::query_as::<_, BanRow>(BY_ID)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await)?;
    Ok(row.map(BanRow::into_ban))
}

/// Bans for a user, newest id first.
pub async fn list_for_user(conn: &mut SqliteConnection, user_id: Id) -> Result<Vec<Ban>, Error> {
    let rows = db(sqlx::query_as::<_, BanRow>(BY_USER)
        .bind(user_id)
        .fetch_all(&mut *conn)
        .await)?;
    Ok(rows.into_iter().map(BanRow::into_ban).collect())
}

/// Sets `lifted_at`.
pub async fn lift(conn: &mut SqliteConnection, id: Id, lifted_at: Timestamp) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE bans SET lifted_at = ? WHERE id = ?",
    )
    .bind(lifted_at.as_millis())
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Deletes a ban.
pub async fn delete(conn: &mut SqliteConnection, id: Id) -> Result<(), Error> {
    changed(db(sqlx::query("DELETE FROM bans WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await)?)
}

#[derive(Debug, sqlx::FromRow)]
struct BanRow {
    id: Id,
    user_id: Id,
    banned_by: Id,
    reason: Option<String>,
    created_at: i64,
    expires_at: Option<i64>,
    lifted_at: Option<i64>,
}

impl BanRow {
    fn into_ban(self) -> Ban {
        Ban {
            id: self.id,
            user_id: self.user_id,
            banned_by: self.banned_by,
            reason: self.reason,
            created_at: stamp(self.created_at),
            expires_at: stamp_opt(self.expires_at),
            lifted_at: stamp_opt(self.lifted_at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NewBan, delete, insert, lift, list_for_user};
    use crate::db::testing;

    #[tokio::test]
    async fn insert_lift_and_delete() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let moderator = testing::user(&mut conn, 1).await;
        let target = testing::user(&mut conn, 2).await;
        let created = insert(
            &mut conn,
            NewBan {
                id: testing::id(3),
                user_id: target.id,
                banned_by: moderator.id,
                reason: Some("spam".into()),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        let lifted_at = ss_core::Timestamp::from_millis(15);
        lift(&mut conn, created.id, lifted_at).await.unwrap();
        assert_eq!(
            list_for_user(&mut conn, target.id).await.unwrap()[0].lifted_at,
            Some(lifted_at)
        );
        delete(&mut conn, created.id).await.unwrap();
        assert_eq!(list_for_user(&mut conn, target.id).await.unwrap().len(), 0);
    }
}
