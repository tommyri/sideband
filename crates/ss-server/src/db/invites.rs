//! `invites` rows.

use sqlx::SqliteConnection;
use ss_core::{Id, Timestamp};

use super::{
    Error,
    convert::{changed, stamp, stamp_opt},
    db,
    model::Invite,
};

/// Fields stored on insert. `uses` starts at zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewInvite {
    /// Primary key.
    pub id: Id,
    /// Unique code.
    pub code: String,
    /// Creator.
    pub created_by: Id,
    /// Optional use cap.
    pub max_uses: Option<i64>,
    /// Optional expiry.
    pub expires_at: Option<Timestamp>,
}

const BY_ID: &str = "SELECT id, code, created_by, max_uses, uses, expires_at, revoked_at, created_at FROM invites WHERE id = ?";
const BY_CODE: &str = "SELECT id, code, created_by, max_uses, uses, expires_at, revoked_at, created_at FROM invites WHERE code = ?";

/// Inserts an invite with `uses = 0`.
pub async fn insert(conn: &mut SqliteConnection, invite: NewInvite) -> Result<Invite, Error> {
    let created_at = Timestamp::now().as_millis();
    db(sqlx::query(
        "INSERT INTO invites (id, code, created_by, max_uses, uses, expires_at, created_at)
         VALUES (?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(invite.id)
    .bind(&invite.code)
    .bind(invite.created_by)
    .bind(invite.max_uses)
    .bind(invite.expires_at.map(Timestamp::as_millis))
    .bind(created_at)
    .execute(&mut *conn)
    .await)?;
    get(conn, invite.id).await?.ok_or(Error::NotFound)
}

/// Loads an invite by id.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<Invite>, Error> {
    let row = db(sqlx::query_as::<_, InviteRow>(BY_ID)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await)?;
    Ok(row.map(InviteRow::into_invite))
}

/// Loads an invite by code.
pub async fn get_by_code(conn: &mut SqliteConnection, code: &str) -> Result<Option<Invite>, Error> {
    let row = db(sqlx::query_as::<_, InviteRow>(BY_CODE)
        .bind(code)
        .fetch_optional(&mut *conn)
        .await)?;
    Ok(row.map(InviteRow::into_invite))
}

/// Adds one to `uses` when the invite is still open.
///
/// A revoked, expired, or exhausted invite is left unchanged and yields `false`.
pub async fn consume(conn: &mut SqliteConnection, id: Id, now: Timestamp) -> Result<bool, Error> {
    let result = db(sqlx::query(
        "UPDATE invites SET uses = uses + 1
         WHERE id = ?
           AND revoked_at IS NULL
           AND (expires_at IS NULL OR expires_at > ?)
           AND (max_uses IS NULL OR uses < max_uses)",
    )
    .bind(id)
    .bind(now.as_millis())
    .execute(&mut *conn)
    .await)?;
    Ok(result.rows_affected() == 1)
}

/// Adds one to `uses`.
pub async fn increment_uses(conn: &mut SqliteConnection, id: Id) -> Result<Invite, Error> {
    changed(db(sqlx::query(
        "UPDATE invites SET uses = uses + 1 WHERE id = ?",
    )
    .bind(id)
    .execute(&mut *conn)
    .await)?)?;
    get(conn, id).await?.ok_or(Error::NotFound)
}

/// Sets `revoked_at`.
pub async fn revoke(
    conn: &mut SqliteConnection,
    id: Id,
    revoked_at: Timestamp,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE invites SET revoked_at = ? WHERE id = ?",
    )
    .bind(revoked_at.as_millis())
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Deletes an invite.
pub async fn delete(conn: &mut SqliteConnection, id: Id) -> Result<(), Error> {
    changed(db(sqlx::query("DELETE FROM invites WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await)?)
}

#[derive(Debug, sqlx::FromRow)]
struct InviteRow {
    id: Id,
    code: String,
    created_by: Id,
    max_uses: Option<i64>,
    uses: i64,
    expires_at: Option<i64>,
    revoked_at: Option<i64>,
    created_at: i64,
}

impl InviteRow {
    fn into_invite(self) -> Invite {
        Invite {
            id: self.id,
            code: self.code,
            created_by: self.created_by,
            max_uses: self.max_uses,
            uses: self.uses,
            expires_at: stamp_opt(self.expires_at),
            revoked_at: stamp_opt(self.revoked_at),
            created_at: stamp(self.created_at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NewInvite, consume, delete, get_by_code, increment_uses, insert, revoke};
    use crate::db::testing;

    #[tokio::test]
    async fn insert_read_increment_revoke_and_delete() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let user = testing::user(&mut conn, 1).await;
        let created = insert(
            &mut conn,
            NewInvite {
                id: testing::id(2),
                code: "welcome".into(),
                created_by: user.id,
                max_uses: Some(5),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(created.uses, 0);
        assert_eq!(
            get_by_code(&mut conn, "welcome").await.unwrap().unwrap().id,
            created.id
        );
        assert_eq!(increment_uses(&mut conn, created.id).await.unwrap().uses, 1);
        let revoked_at = ss_core::Timestamp::from_millis(9);
        revoke(&mut conn, created.id, revoked_at).await.unwrap();
        assert_eq!(
            get_by_code(&mut conn, "welcome")
                .await
                .unwrap()
                .unwrap()
                .revoked_at,
            Some(revoked_at)
        );
        delete(&mut conn, created.id).await.unwrap();
        assert!(get_by_code(&mut conn, "welcome").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn consume_stops_at_the_cap_and_after_revocation_or_expiry() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let user = testing::user(&mut conn, 1).await;
        let now = ss_core::Timestamp::from_millis(1_000);
        let open = insert(
            &mut conn,
            NewInvite {
                id: testing::id(2),
                code: "once".into(),
                created_by: user.id,
                max_uses: Some(1),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        assert!(consume(&mut conn, open.id, now).await.unwrap());
        assert!(!consume(&mut conn, open.id, now).await.unwrap());
        let expired = insert(
            &mut conn,
            NewInvite {
                id: testing::id(3),
                code: "old".into(),
                created_by: user.id,
                max_uses: None,
                expires_at: Some(ss_core::Timestamp::from_millis(10)),
            },
        )
        .await
        .unwrap();
        assert!(!consume(&mut conn, expired.id, now).await.unwrap());
        let revoked = insert(
            &mut conn,
            NewInvite {
                id: testing::id(4),
                code: "gone".into(),
                created_by: user.id,
                max_uses: None,
                expires_at: None,
            },
        )
        .await
        .unwrap();
        revoke(&mut conn, revoked.id, now).await.unwrap();
        assert!(!consume(&mut conn, revoked.id, now).await.unwrap());
    }
}
