//! `users` rows.

use sqlx::SqliteConnection;
use ss_core::{Id, Timestamp, UserKind};

use super::{
    Error,
    convert::{changed, stamp, stamp_opt},
    db,
    model::User,
};

/// Fields stored on insert. The repository sets `created_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUser {
    /// Primary key.
    pub id: Id,
    /// Unique login name.
    pub username: String,
    /// Display name.
    pub display_name: String,
    /// Already-hashed password.
    pub password_hash: String,
    /// Human or bot.
    pub kind: UserKind,
}

/// Display name and avatar written by [`set_profile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProfile {
    /// Display name.
    pub display_name: String,
    /// Avatar attachment, or `None` to clear it.
    pub avatar_attachment_id: Option<Id>,
}

const BY_ID: &str = "SELECT id, username, display_name, password_hash, avatar_attachment_id, kind, created_at, disabled_at FROM users WHERE id = ?";
const BY_USERNAME: &str = "SELECT id, username, display_name, password_hash, avatar_attachment_id, kind, created_at, disabled_at FROM users WHERE username = ?";
const ALL_USERS: &str = "SELECT id, username, display_name, password_hash, avatar_attachment_id, kind, created_at, disabled_at FROM users ORDER BY username, id";

/// Inserts a user.
pub async fn insert(conn: &mut SqliteConnection, user: NewUser) -> Result<User, Error> {
    let kind = user.kind.as_i64();
    let created_at = Timestamp::now().as_millis();
    db(sqlx::query(
        "INSERT INTO users (id, username, display_name, password_hash, kind, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(user.id)
    .bind(&user.username)
    .bind(&user.display_name)
    .bind(&user.password_hash)
    .bind(kind)
    .bind(created_at)
    .execute(&mut *conn)
    .await)?;
    get(conn, user.id).await?.ok_or(Error::NotFound)
}

/// Loads a user by id, including disabled accounts.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<User>, Error> {
    let row = db(sqlx::query_as::<_, UserRow>(BY_ID)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await)?;
    row.map(UserRow::into_user).transpose()
}

/// Every user, including disabled accounts, ordered by username.
pub async fn list(conn: &mut SqliteConnection) -> Result<Vec<User>, Error> {
    let rows = db(sqlx::query_as::<_, UserRow>(ALL_USERS)
        .fetch_all(&mut *conn)
        .await)?;
    rows.into_iter().map(UserRow::into_user).collect()
}

/// Loads a user by username. The comparison is case-insensitive.
pub async fn get_by_username(
    conn: &mut SqliteConnection,
    username: &str,
) -> Result<Option<User>, Error> {
    let row = db(sqlx::query_as::<_, UserRow>(BY_USERNAME)
        .bind(username)
        .fetch_optional(&mut *conn)
        .await)?;
    row.map(UserRow::into_user).transpose()
}

/// Replaces the display name and avatar.
pub async fn set_profile(
    conn: &mut SqliteConnection,
    id: Id,
    profile: UserProfile,
) -> Result<User, Error> {
    changed(db(sqlx::query(
        "UPDATE users SET display_name = ?, avatar_attachment_id = ? WHERE id = ?",
    )
    .bind(&profile.display_name)
    .bind(profile.avatar_attachment_id)
    .bind(id)
    .execute(&mut *conn)
    .await)?)?;
    get(conn, id).await?.ok_or(Error::NotFound)
}

/// Replaces the stored password hash.
pub async fn set_password(
    conn: &mut SqliteConnection,
    id: Id,
    password_hash: &str,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE users SET password_hash = ? WHERE id = ?",
    )
    .bind(password_hash)
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Sets or clears `disabled_at`.
pub async fn set_disabled(
    conn: &mut SqliteConnection,
    id: Id,
    disabled_at: Option<Timestamp>,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE users SET disabled_at = ? WHERE id = ?",
    )
    .bind(disabled_at.map(Timestamp::as_millis))
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Number of accounts, including disabled ones.
pub async fn count(conn: &mut SqliteConnection) -> Result<i64, Error> {
    db(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
        .fetch_one(&mut *conn)
        .await)
}

/// Deletes a user who has no dependent rows.
pub async fn delete(conn: &mut SqliteConnection, id: Id) -> Result<(), Error> {
    changed(db(sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await)?)
}

#[derive(Debug, sqlx::FromRow)]
struct UserRow {
    id: Id,
    username: String,
    display_name: String,
    password_hash: String,
    avatar_attachment_id: Option<Id>,
    kind: i64,
    created_at: i64,
    disabled_at: Option<i64>,
}

impl UserRow {
    fn into_user(self) -> Result<User, Error> {
        Ok(User {
            id: self.id,
            username: self.username,
            display_name: self.display_name,
            password_hash: self.password_hash,
            avatar_attachment_id: self.avatar_attachment_id,
            kind: UserKind::try_from(self.kind)?,
            created_at: stamp(self.created_at),
            disabled_at: stamp_opt(self.disabled_at),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        NewUser, UserProfile, count, delete, get, get_by_username, insert, set_disabled,
        set_password, set_profile,
    };
    use crate::db::{Error, testing};

    #[tokio::test]
    async fn insert_read_update_and_delete() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let created = insert(
            &mut conn,
            NewUser {
                id: testing::id(1),
                username: "Ada".into(),
                display_name: "Ada".into(),
                password_hash: "one".into(),
                kind: ss_core::UserKind::User,
            },
        )
        .await
        .unwrap();
        assert_eq!(get(&mut conn, created.id).await.unwrap().unwrap(), created);
        assert_eq!(
            get_by_username(&mut conn, "ada").await.unwrap().unwrap().id,
            created.id
        );
        let updated = set_profile(
            &mut conn,
            created.id,
            UserProfile {
                display_name: "Ada Lovelace".into(),
                avatar_attachment_id: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(updated.display_name, "Ada Lovelace");
        set_password(&mut conn, created.id, "two").await.unwrap();
        assert_eq!(
            get(&mut conn, created.id)
                .await
                .unwrap()
                .unwrap()
                .password_hash,
            "two"
        );
        let disabled_at = ss_core::Timestamp::from_millis(10);
        set_disabled(&mut conn, created.id, Some(disabled_at))
            .await
            .unwrap();
        assert_eq!(
            get(&mut conn, created.id)
                .await
                .unwrap()
                .unwrap()
                .disabled_at,
            Some(disabled_at)
        );
        let duplicate = insert(
            &mut conn,
            NewUser {
                id: testing::id(2),
                username: "ADA".into(),
                display_name: "Other".into(),
                password_hash: "x".into(),
                kind: ss_core::UserKind::Bot,
            },
        )
        .await;
        assert!(matches!(duplicate, Err(Error::Conflict)));
        assert_eq!(count(&mut conn).await.unwrap(), 1);
        delete(&mut conn, created.id).await.unwrap();
        assert_eq!(count(&mut conn).await.unwrap(), 0);
        assert!(get(&mut conn, created.id).await.unwrap().is_none());
        assert!(matches!(
            delete(&mut conn, created.id).await,
            Err(Error::NotFound)
        ));
    }
}
