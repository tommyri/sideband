//! `sessions` rows.

use sqlx::SqliteConnection;
use ss_core::{Id, Timestamp};

use super::{
    Error,
    convert::{changed, stamp, stamp_opt},
    db,
    model::Session,
};

/// Fields stored on insert. `created_at` and `last_seen_at` start at the same time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    /// Primary key.
    pub id: Id,
    /// Account.
    pub user_id: Id,
    /// Hash of the bearer token.
    pub token_hash: String,
    /// Device label.
    pub device_name: String,
    /// Optional expiry.
    pub expires_at: Option<Timestamp>,
}

const BY_ID: &str = "SELECT id, user_id, token_hash, device_name, created_at, last_seen_at, expires_at FROM sessions WHERE id = ?";
const BY_TOKEN: &str = "SELECT id, user_id, token_hash, device_name, created_at, last_seen_at, expires_at FROM sessions WHERE token_hash = ?";
const BY_USER: &str = "SELECT id, user_id, token_hash, device_name, created_at, last_seen_at, expires_at FROM sessions WHERE user_id = ? ORDER BY id";

/// Inserts a session.
pub async fn insert(conn: &mut SqliteConnection, session: NewSession) -> Result<Session, Error> {
    let now = Timestamp::now().as_millis();
    db(sqlx::query(
        "INSERT INTO sessions (id, user_id, token_hash, device_name, created_at, last_seen_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(session.id)
    .bind(session.user_id)
    .bind(&session.token_hash)
    .bind(&session.device_name)
    .bind(now)
    .bind(now)
    .bind(session.expires_at.map(Timestamp::as_millis))
    .execute(&mut *conn)
    .await)?;
    get(conn, session.id).await?.ok_or(Error::NotFound)
}

/// Loads a session by id.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<Session>, Error> {
    load_one(conn, BY_ID, id).await
}

/// Loads a session by token hash.
pub async fn get_by_token_hash(
    conn: &mut SqliteConnection,
    token_hash: &str,
) -> Result<Option<Session>, Error> {
    let row = db(sqlx::query_as::<_, SessionRow>(BY_TOKEN)
        .bind(token_hash)
        .fetch_optional(&mut *conn)
        .await)?;
    Ok(row.map(SessionRow::into_session))
}

/// Sessions for one user, oldest first.
pub async fn list_for_user(
    conn: &mut SqliteConnection,
    user_id: Id,
) -> Result<Vec<Session>, Error> {
    let rows = db(sqlx::query_as::<_, SessionRow>(BY_USER)
        .bind(user_id)
        .fetch_all(&mut *conn)
        .await)?;
    Ok(rows.into_iter().map(SessionRow::into_session).collect())
}

/// Sets `last_seen_at`.
pub async fn touch(
    conn: &mut SqliteConnection,
    id: Id,
    last_seen_at: Timestamp,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE sessions SET last_seen_at = ? WHERE id = ?",
    )
    .bind(last_seen_at.as_millis())
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Sets `last_seen_at` and `expires_at` together.
pub async fn slide(
    conn: &mut SqliteConnection,
    id: Id,
    last_seen_at: Timestamp,
    expires_at: Timestamp,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE sessions SET last_seen_at = ?, expires_at = ? WHERE id = ?",
    )
    .bind(last_seen_at.as_millis())
    .bind(expires_at.as_millis())
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Deletes a session.
pub async fn delete(conn: &mut SqliteConnection, id: Id) -> Result<(), Error> {
    changed(db(sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await)?)
}

async fn load_one(
    conn: &mut SqliteConnection,
    sql: &'static str,
    id: Id,
) -> Result<Option<Session>, Error> {
    let row = db(sqlx::query_as::<_, SessionRow>(sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await)?;
    Ok(row.map(SessionRow::into_session))
}

#[derive(Debug, sqlx::FromRow)]
struct SessionRow {
    id: Id,
    user_id: Id,
    token_hash: String,
    device_name: String,
    created_at: i64,
    last_seen_at: i64,
    expires_at: Option<i64>,
}

impl SessionRow {
    fn into_session(self) -> Session {
        Session {
            id: self.id,
            user_id: self.user_id,
            token_hash: self.token_hash,
            device_name: self.device_name,
            created_at: stamp(self.created_at),
            last_seen_at: stamp(self.last_seen_at),
            expires_at: stamp_opt(self.expires_at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NewSession, delete, get_by_token_hash, insert, list_for_user, slide, touch};
    use crate::db::{Error, testing};

    #[tokio::test]
    async fn insert_read_touch_and_delete() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let user = testing::user(&mut conn, 1).await;
        let created = insert(
            &mut conn,
            NewSession {
                id: testing::id(2),
                user_id: user.id,
                token_hash: "token".into(),
                device_name: "laptop".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            get_by_token_hash(&mut conn, "token")
                .await
                .unwrap()
                .unwrap()
                .id,
            created.id
        );
        let seen = ss_core::Timestamp::from_millis(50);
        touch(&mut conn, created.id, seen).await.unwrap();
        let expires = ss_core::Timestamp::from_millis(90);
        slide(&mut conn, created.id, seen, expires).await.unwrap();
        let stored = &list_for_user(&mut conn, user.id).await.unwrap()[0];
        assert_eq!(stored.last_seen_at, seen);
        assert_eq!(stored.expires_at, Some(expires));
        let duplicate = insert(
            &mut conn,
            NewSession {
                id: testing::id(3),
                user_id: user.id,
                token_hash: "token".into(),
                device_name: "phone".into(),
                expires_at: None,
            },
        )
        .await;
        assert!(matches!(duplicate, Err(Error::Conflict)));
        delete(&mut conn, created.id).await.unwrap();
        assert_eq!(list_for_user(&mut conn, user.id).await.unwrap().len(), 0);
    }
}
