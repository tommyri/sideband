//! `bot_tokens` rows. A bot is a `users` row with kind 1.

use sqlx::SqliteConnection;
use ss_core::{Id, Permissions, Timestamp};

use super::{
    Error,
    convert::{permissions_from_i64, permissions_to_i64, stamp, stamp_opt},
    db,
};

/// One bot token. The plaintext is never stored.
#[derive(Clone, PartialEq, Eq)]
pub struct BotToken {
    /// Primary key. Connections that used the token are tracked by it.
    pub id: Id,
    /// The bot account.
    pub bot_id: Id,
    /// Hex SHA-256 of the token.
    pub token_hash: String,
    /// Bits the token may use, on top of the bot's roles.
    pub scopes: Permissions,
    /// Member who issued the token. `None` for the operator CLI.
    pub created_by: Option<Id>,
    /// When it was issued.
    pub created_at: Timestamp,
    /// When it was revoked or replaced. `None` while live.
    pub revoked_at: Option<Timestamp>,
}

impl std::fmt::Debug for BotToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotToken")
            .field("id", &self.id)
            .field("bot_id", &self.bot_id)
            .field("token_hash", &super::REDACTED)
            .field("scopes", &self.scopes)
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("revoked_at", &self.revoked_at)
            .finish()
    }
}

/// Fields stored on insert. The repository sets `created_at`.
#[derive(Clone, PartialEq, Eq)]
pub struct NewBotToken {
    /// Primary key.
    pub id: Id,
    /// The bot account.
    pub bot_id: Id,
    /// Hex SHA-256 of the token.
    pub token_hash: String,
    /// Bits the token may use.
    pub scopes: Permissions,
    /// Member who issued the token. `None` for the operator CLI.
    pub created_by: Option<Id>,
}

/// Inserts a live token. A bot with a live token already is a conflict:
/// revoke the old one first, in the same transaction.
pub async fn insert(conn: &mut SqliteConnection, token: NewBotToken) -> Result<BotToken, Error> {
    db(sqlx::query(
        "INSERT INTO bot_tokens (id, bot_id, token_hash, scopes, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(token.id)
    .bind(token.bot_id)
    .bind(&token.token_hash)
    .bind(permissions_to_i64(token.scopes)?)
    .bind(token.created_by)
    .bind(Timestamp::now().as_millis())
    .execute(&mut *conn)
    .await)?;
    get(conn, token.id).await?.ok_or(Error::NotFound)
}

/// Loads a token by id, live or not.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<BotToken>, Error> {
    let row = db(
        sqlx::query_as::<_, Row>("SELECT id, bot_id, token_hash, scopes, created_by, created_at, revoked_at FROM bot_tokens WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await,
    )?;
    row.map(Row::into_token).transpose()
}

/// The live token with this hash, if any.
pub async fn live_by_hash(
    conn: &mut SqliteConnection,
    token_hash: &str,
) -> Result<Option<BotToken>, Error> {
    let row = db(sqlx::query_as::<_, Row>("SELECT id, bot_id, token_hash, scopes, created_by, created_at, revoked_at FROM bot_tokens WHERE token_hash = ? AND revoked_at IS NULL")
    .bind(token_hash)
    .fetch_optional(&mut *conn)
    .await)?;
    row.map(Row::into_token).transpose()
}

/// The bot's live token, if any.
pub async fn live_for(conn: &mut SqliteConnection, bot: Id) -> Result<Option<BotToken>, Error> {
    let row = db(sqlx::query_as::<_, Row>("SELECT id, bot_id, token_hash, scopes, created_by, created_at, revoked_at FROM bot_tokens WHERE bot_id = ? AND revoked_at IS NULL")
    .bind(bot)
    .fetch_optional(&mut *conn)
    .await)?;
    row.map(Row::into_token).transpose()
}

/// Every live token.
pub async fn live(conn: &mut SqliteConnection) -> Result<Vec<BotToken>, Error> {
    let rows = db(sqlx::query_as::<_, Row>("SELECT id, bot_id, token_hash, scopes, created_by, created_at, revoked_at FROM bot_tokens WHERE revoked_at IS NULL ORDER BY bot_id")
    .fetch_all(&mut *conn)
    .await)?;
    rows.into_iter().map(Row::into_token).collect()
}

/// Every bot account with its live token's id and scopes, or `None` when it
/// has none. The permission snapshot caps bots by this.
pub async fn scopes(
    conn: &mut SqliteConnection,
) -> Result<Vec<(Id, Option<(Id, Permissions)>)>, Error> {
    let rows: Vec<(Id, Option<Id>, Option<i64>)> = db(sqlx::query_as(
        "SELECT u.id, t.id, t.scopes FROM users u
         LEFT JOIN bot_tokens t ON t.bot_id = u.id AND t.revoked_at IS NULL
         WHERE u.kind = 1",
    )
    .fetch_all(&mut *conn)
    .await)?;
    rows.into_iter()
        .map(|(id, token, scopes)| {
            let live = match (token, scopes) {
                (Some(token), Some(scopes)) => Some((token, permissions_from_i64(scopes)?)),
                _ => None,
            };
            Ok((id, live))
        })
        .collect()
}

/// Revokes the bot's live token. Returns its id, or `None` when it had none.
pub async fn revoke_for(
    conn: &mut SqliteConnection,
    bot: Id,
    at: Timestamp,
) -> Result<Option<Id>, Error> {
    let Some(token) = live_for(conn, bot).await? else {
        return Ok(None);
    };
    db(
        sqlx::query("UPDATE bot_tokens SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
            .bind(at.as_millis())
            .bind(token.id)
            .execute(&mut *conn)
            .await,
    )?;
    Ok(Some(token.id))
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Id,
    bot_id: Id,
    token_hash: String,
    scopes: i64,
    created_by: Option<Id>,
    created_at: i64,
    revoked_at: Option<i64>,
}

impl Row {
    fn into_token(self) -> Result<BotToken, Error> {
        Ok(BotToken {
            id: self.id,
            bot_id: self.bot_id,
            token_hash: self.token_hash,
            scopes: permissions_from_i64(self.scopes)?,
            created_by: self.created_by,
            created_at: stamp(self.created_at),
            revoked_at: stamp_opt(self.revoked_at),
        })
    }
}

#[cfg(test)]
mod tests {
    use ss_core::{Permissions, Timestamp};

    use super::{NewBotToken, insert, live_by_hash, live_for, revoke_for};
    use crate::db::{Error, testing};

    #[tokio::test]
    async fn a_bot_has_one_live_token_and_a_revoked_one_is_not_found() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let owner = testing::user(&mut conn, 1).await;
        let bot = testing::user(&mut conn, 2).await;
        let token = |id, hash: &str| NewBotToken {
            id: testing::id(id),
            bot_id: bot.id,
            token_hash: hash.into(),
            scopes: Permissions::SEND_MESSAGES,
            created_by: Some(owner.id),
        };
        insert(&mut conn, token(3, "first")).await.unwrap();
        assert!(matches!(
            insert(&mut conn, token(4, "second")).await,
            Err(Error::Conflict)
        ));
        let revoked = revoke_for(&mut conn, bot.id, Timestamp::from_millis(5))
            .await
            .unwrap();
        assert_eq!(revoked, Some(testing::id(3)));
        assert!(live_by_hash(&mut conn, "first").await.unwrap().is_none());
        insert(&mut conn, token(4, "second")).await.unwrap();
        let live = live_for(&mut conn, bot.id).await.unwrap().unwrap();
        assert_eq!(live.id, testing::id(4));
        assert_eq!(live.scopes, Permissions::SEND_MESSAGES);
    }
}
