//! `server_settings` rows.
//!
//! The migration seeds [`NAME`], [`DESCRIPTION`], [`ICON`], and
//! [`REGISTRATION_MODE`]. Registration starts as [`REGISTRATION_INVITE`].

use sqlx::SqliteConnection;

use super::{Error, convert::changed, db, model::Setting};

/// Seeded server name key.
pub const NAME: &str = "name";
/// Seeded server description key.
pub const DESCRIPTION: &str = "description";
/// Seeded icon key. The value is an attachment id or empty.
pub const ICON: &str = "icon";
/// Seeded registration policy key.
pub const REGISTRATION_MODE: &str = "registration_mode";
/// Invite-only registration.
pub const REGISTRATION_INVITE: &str = "invite";

/// Loads one setting.
pub async fn get(conn: &mut SqliteConnection, key: &str) -> Result<Option<Setting>, Error> {
    let row = db(sqlx::query_as::<_, SettingRow>(
        "SELECT key, value FROM server_settings WHERE key = ?",
    )
    .bind(key)
    .fetch_optional(&mut *conn)
    .await)?;
    Ok(row.map(SettingRow::into_setting))
}

/// Inserts or replaces a setting.
pub async fn set(conn: &mut SqliteConnection, key: &str, value: &str) -> Result<Setting, Error> {
    db(sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES (?, ?)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(&mut *conn)
    .await)?;
    get(conn, key).await?.ok_or(Error::NotFound)
}

/// Deletes a setting only when it still holds `value`.
pub async fn take(conn: &mut SqliteConnection, key: &str, value: &str) -> Result<bool, Error> {
    let result = db(
        sqlx::query("DELETE FROM server_settings WHERE key = ? AND value = ?")
            .bind(key)
            .bind(value)
            .execute(&mut *conn)
            .await,
    )?;
    Ok(result.rows_affected() == 1)
}

/// Deletes a setting.
pub async fn delete(conn: &mut SqliteConnection, key: &str) -> Result<(), Error> {
    changed(db(sqlx::query(
        "DELETE FROM server_settings WHERE key = ?",
    )
    .bind(key)
    .execute(&mut *conn)
    .await)?)
}

/// Every setting, ordered by key.
pub async fn list(conn: &mut SqliteConnection) -> Result<Vec<Setting>, Error> {
    let rows = db(sqlx::query_as::<_, SettingRow>(
        "SELECT key, value FROM server_settings ORDER BY key",
    )
    .fetch_all(&mut *conn)
    .await)?;
    Ok(rows.into_iter().map(SettingRow::into_setting).collect())
}

#[derive(Debug, sqlx::FromRow)]
struct SettingRow {
    key: String,
    value: String,
}

impl SettingRow {
    fn into_setting(self) -> Setting {
        Setting {
            key: self.key,
            value: self.value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DESCRIPTION, ICON, NAME, REGISTRATION_INVITE, REGISTRATION_MODE, delete, get, list, set,
        take,
    };
    use crate::db::testing;

    #[tokio::test]
    async fn seeds_and_updates() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        assert_eq!(
            get(&mut conn, NAME).await.unwrap().unwrap().value,
            "SocialSpeak"
        );
        assert_eq!(
            get(&mut conn, DESCRIPTION).await.unwrap().unwrap().value,
            ""
        );
        assert_eq!(get(&mut conn, ICON).await.unwrap().unwrap().value, "");
        assert_eq!(
            get(&mut conn, REGISTRATION_MODE)
                .await
                .unwrap()
                .unwrap()
                .value,
            REGISTRATION_INVITE
        );
        set(&mut conn, NAME, "Friends").await.unwrap();
        assert_eq!(
            get(&mut conn, NAME).await.unwrap().unwrap().value,
            "Friends"
        );
        set(&mut conn, "motd", "hello").await.unwrap();
        assert_eq!(list(&mut conn).await.unwrap().len(), 5);
        assert!(!take(&mut conn, "motd", "other").await.unwrap());
        assert!(take(&mut conn, "motd", "hello").await.unwrap());
        assert!(get(&mut conn, "motd").await.unwrap().is_none());
        delete(&mut conn, "motd").await.unwrap_err();
    }
}
