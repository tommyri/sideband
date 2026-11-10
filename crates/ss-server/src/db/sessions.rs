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

/// Deletes `id` only when it belongs to `user_id`. `false` when no row matched.
pub async fn delete_owned(conn: &mut SqliteConnection, id: Id, user_id: Id) -> Result<bool, Error> {
    let done = db(
        sqlx::query("DELETE FROM sessions WHERE id = ? AND user_id = ?")
            .bind(id)
            .bind(user_id)
            .execute(&mut *conn)
            .await,
    )?;
    Ok(done.rows_affected() > 0)
}

/// Deletes every session of one account. Returns how many there were.
pub async fn delete_for_user(conn: &mut SqliteConnection, user_id: Id) -> Result<u64, Error> {
    let done = db(sqlx::query("DELETE FROM sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *conn)
        .await)?;
    Ok(done.rows_affected())
}

/// Deletes sessions whose sliding expiry is at or before `now`, or that were
/// created at or before `created_before` (the absolute lifetime). Returns
/// how many rows went.
pub async fn delete_expired(
    conn: &mut SqliteConnection,
    now: Timestamp,
    created_before: Timestamp,
) -> Result<u64, Error> {
    let done = db(sqlx::query(
        "DELETE FROM sessions WHERE (expires_at IS NOT NULL AND expires_at <= ?) OR created_at <= ?",
    )
    .bind(now.as_millis())
    .bind(created_before.as_millis())
    .execute(&mut *conn)
    .await)?;
    Ok(done.rows_affected())
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
    use super::{
        NewSession, delete, delete_expired, delete_for_user, delete_owned, get_by_token_hash,
        insert, list_for_user, slide, touch,
    };
    use crate::db::{Error, testing};
    use ss_core::{Id, Timestamp};

    fn session(id: Id, user_id: Id, token: &str, expires: Option<i64>) -> NewSession {
        NewSession {
            id,
            user_id,
            token_hash: token.into(),
            device_name: "laptop".into(),
            expires_at: expires.map(Timestamp::from_millis),
        }
    }

    #[tokio::test]
    async fn owned_user_and_expired_deletes_touch_only_their_rows() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let ada = testing::user(&mut conn, 1).await;
        let bob = testing::user(&mut conn, 2).await;
        let now = Timestamp::now().as_millis();
        insert(
            &mut conn,
            session(testing::id(10), ada.id, "a", Some(now + 1000)),
        )
        .await
        .unwrap();
        insert(
            &mut conn,
            session(testing::id(11), ada.id, "b", Some(now - 1)),
        )
        .await
        .unwrap();
        insert(&mut conn, session(testing::id(12), bob.id, "c", None))
            .await
            .unwrap();
        assert!(
            !delete_owned(&mut conn, testing::id(12), ada.id)
                .await
                .unwrap()
        );
        let cutoff = Timestamp::from_millis(now - 1_000_000);
        assert_eq!(
            delete_expired(&mut conn, Timestamp::from_millis(now), cutoff)
                .await
                .unwrap(),
            1
        );
        let left: Vec<_> = list_for_user(&mut conn, ada.id)
            .await
            .unwrap()
            .into_iter()
            .map(|session| session.id)
            .collect();
        assert_eq!(left, vec![testing::id(10)]);
        // Created "now" is after the cutoff; a cutoff in the future takes it.
        let later = Timestamp::from_millis(now + 10_000);
        assert_eq!(
            delete_expired(&mut conn, Timestamp::from_millis(now), later)
                .await
                .unwrap(),
            2
        );
        insert(&mut conn, session(testing::id(13), ada.id, "d", None))
            .await
            .unwrap();
        insert(&mut conn, session(testing::id(14), ada.id, "e", None))
            .await
            .unwrap();
        assert!(
            delete_owned(&mut conn, testing::id(13), ada.id)
                .await
                .unwrap()
        );
        assert_eq!(delete_for_user(&mut conn, ada.id).await.unwrap(), 1);
        assert_eq!(delete_for_user(&mut conn, ada.id).await.unwrap(), 0);
    }

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
