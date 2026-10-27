//! `read_states` rows.

use sqlx::SqliteConnection;
use ss_core::Id;

use super::{Error, convert::changed, db, model::ReadState};

/// Inserts or replaces the read cursor for a user in a channel.
pub async fn upsert(
    conn: &mut SqliteConnection,
    user_id: Id,
    channel_id: Id,
    last_read_message_id: Id,
) -> Result<ReadState, Error> {
    db(sqlx::query(
        "INSERT INTO read_states (user_id, channel_id, last_read_message_id) VALUES (?, ?, ?)
         ON CONFLICT (user_id, channel_id) DO UPDATE SET last_read_message_id = excluded.last_read_message_id",
    )
    .bind(user_id)
    .bind(channel_id)
    .bind(last_read_message_id)
    .execute(&mut *conn)
    .await)?;
    get(conn, user_id, channel_id).await?.ok_or(Error::NotFound)
}

/// Loads one read cursor.
pub async fn get(
    conn: &mut SqliteConnection,
    user_id: Id,
    channel_id: Id,
) -> Result<Option<ReadState>, Error> {
    let row = db(sqlx::query_as::<_, ReadStateRow>(
        "SELECT user_id, channel_id, last_read_message_id FROM read_states WHERE user_id = ? AND channel_id = ?",
    )
    .bind(user_id)
    .bind(channel_id)
    .fetch_optional(&mut *conn)
    .await)?;
    Ok(row.map(ReadStateRow::into_state))
}

#[derive(Debug, sqlx::FromRow)]
#[allow(
    clippy::struct_field_names,
    reason = "the fields are the read_states column names"
)]
struct ReadStateRow {
    user_id: Id,
    channel_id: Id,
    last_read_message_id: Id,
}

impl ReadStateRow {
    fn into_state(self) -> ReadState {
        ReadState {
            user_id: self.user_id,
            channel_id: self.channel_id,
            last_read_message_id: self.last_read_message_id,
        }
    }
}

/// Deletes a read cursor.
pub async fn delete(conn: &mut SqliteConnection, user_id: Id, channel_id: Id) -> Result<(), Error> {
    changed(db(sqlx::query(
        "DELETE FROM read_states WHERE user_id = ? AND channel_id = ?",
    )
    .bind(user_id)
    .bind(channel_id)
    .execute(&mut *conn)
    .await)?)
}

#[cfg(test)]
mod tests {
    use super::{delete, get, upsert};
    use crate::db::testing;

    #[tokio::test]
    async fn upsert_and_delete() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let user = testing::user(&mut conn, 1).await;
        let channel = testing::text_channel(&mut conn, 2, user.id).await;
        let first = testing::message(&mut conn, 3, channel.id, user.id).await;
        let second = testing::message(&mut conn, 4, channel.id, user.id).await;
        upsert(&mut conn, user.id, channel.id, first.id)
            .await
            .unwrap();
        let state = upsert(&mut conn, user.id, channel.id, second.id)
            .await
            .unwrap();
        assert_eq!(state.last_read_message_id, second.id);
        assert_eq!(
            get(&mut conn, user.id, channel.id)
                .await
                .unwrap()
                .unwrap()
                .last_read_message_id,
            second.id
        );
        delete(&mut conn, user.id, channel.id).await.unwrap();
        assert!(get(&mut conn, user.id, channel.id).await.unwrap().is_none());
    }
}
