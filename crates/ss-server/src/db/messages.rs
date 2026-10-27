//! `messages` and `message_mentions` rows.
//!
//! Pages return non-deleted messages in ascending id order. [`page_before`]
//! takes the newest `limit` rows strictly older than the cursor. [`page_around`]
//! keeps the anchor when it is visible and fills with neighbors, at most half
//! of the spare slots older than the anchor. A missing anchor returns the
//! `limit` nearest visible ids, preferring the older id when the distances match.

use std::collections::VecDeque;

use sqlx::{Connection, SqliteConnection};
use ss_core::{Id, Timestamp};

use super::{
    Error,
    convert::{changed, stamp, stamp_opt},
    db,
    model::Message,
};

/// Fields stored on insert. The repository sets `created_at` and writes mentions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    /// Primary key.
    pub id: Id,
    /// Channel.
    pub channel_id: Id,
    /// Author. Empty when a webhook posts alone.
    pub author_id: Option<Id>,
    /// Webhook, when one posted the message.
    pub webhook_id: Option<Id>,
    /// Name shown for a webhook post.
    pub webhook_name: Option<String>,
    /// Body.
    pub content: String,
    /// Replied-to message.
    pub reply_to: Option<Id>,
    /// Mentioned users. Duplicates are ignored.
    pub mentions: Vec<Id>,
    /// Authorized `@everyone` ping.
    pub mentions_everyone: bool,
}

const BY_ID: &str = "SELECT id, channel_id, author_id, webhook_id, webhook_name, content, reply_to, mentions_everyone, created_at, edited_at, deleted_at FROM messages WHERE id = ?";
const NEWEST: &str = "SELECT id, channel_id, author_id, webhook_id, webhook_name, content, reply_to, mentions_everyone, created_at, edited_at, deleted_at FROM messages WHERE channel_id = ? AND deleted_at IS NULL ORDER BY id DESC LIMIT ?";
const OLDER: &str = "SELECT id, channel_id, author_id, webhook_id, webhook_name, content, reply_to, mentions_everyone, created_at, edited_at, deleted_at FROM messages WHERE channel_id = ? AND id < ? AND deleted_at IS NULL ORDER BY id DESC LIMIT ?";
const NEWER: &str = "SELECT id, channel_id, author_id, webhook_id, webhook_name, content, reply_to, mentions_everyone, created_at, edited_at, deleted_at FROM messages WHERE channel_id = ? AND id > ? AND deleted_at IS NULL ORDER BY id ASC LIMIT ?";

/// Inserts a message and its mentions.
pub async fn insert(conn: &mut SqliteConnection, message: NewMessage) -> Result<Message, Error> {
    let created_at = Timestamp::now().as_millis();
    let mut tx = db(conn.begin().await)?;
    db(sqlx::query(
        "INSERT INTO messages
         (id, channel_id, author_id, webhook_id, webhook_name, content, reply_to, created_at, mentions_everyone)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(message.id)
    .bind(message.channel_id)
    .bind(message.author_id)
    .bind(message.webhook_id)
    .bind(&message.webhook_name)
    .bind(&message.content)
    .bind(message.reply_to)
    .bind(created_at)
    .bind(i64::from(message.mentions_everyone))
    .execute(&mut *tx)
    .await)?;
    replace_mentions(&mut tx, message.id, &message.mentions).await?;
    db(tx.commit().await)?;
    get(conn, message.id).await?.ok_or(Error::NotFound)
}

/// Loads a message by id, including soft-deleted rows.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<Message>, Error> {
    let row = db(sqlx::query_as::<_, MessageRow>(BY_ID)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await)?;
    Ok(row.map(MessageRow::into_message))
}

/// Replaces the body and mentions, and sets `edited_at`.
pub async fn edit(
    conn: &mut SqliteConnection,
    id: Id,
    content: &str,
    mentions: &[Id],
    mentions_everyone: bool,
    edited_at: Timestamp,
) -> Result<Message, Error> {
    let mut tx = db(conn.begin().await)?;
    changed(db(sqlx::query(
        "UPDATE messages SET content = ?, edited_at = ?, mentions_everyone = ? WHERE id = ?",
    )
    .bind(content)
    .bind(edited_at.as_millis())
    .bind(i64::from(mentions_everyone))
    .bind(id)
    .execute(&mut *tx)
    .await)?)?;
    replace_mentions(&mut tx, id, mentions).await?;
    db(tx.commit().await)?;
    get(conn, id).await?.ok_or(Error::NotFound)
}

/// Sets `deleted_at`. The row stays, and pages skip it.
pub async fn soft_delete(
    conn: &mut SqliteConnection,
    id: Id,
    deleted_at: Timestamp,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE messages SET deleted_at = ? WHERE id = ?",
    )
    .bind(deleted_at.as_millis())
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Visible messages after `after` that ping `user`, excluding ones `user` wrote.
pub async fn unread_mentions(
    conn: &mut SqliteConnection,
    channel_id: Id,
    user_id: Id,
    after: Id,
) -> Result<u32, Error> {
    let count: i64 = db(sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages
         WHERE channel_id = ? AND deleted_at IS NULL AND id > ?
           AND (author_id IS NULL OR author_id != ?)
           AND (mentions_everyone != 0 OR id IN (
                SELECT message_id FROM message_mentions WHERE user_id = ?))",
    )
    .bind(channel_id)
    .bind(after)
    .bind(user_id)
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await)?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Mentioned users, ordered by id.
pub async fn mentions(conn: &mut SqliteConnection, id: Id) -> Result<Vec<Id>, Error> {
    db(sqlx::query_scalar(
        "SELECT user_id FROM message_mentions WHERE message_id = ? ORDER BY user_id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await)
}

/// Newest visible message in a channel.
pub async fn latest_id(conn: &mut SqliteConnection, channel_id: Id) -> Result<Option<Id>, Error> {
    db(sqlx::query_scalar(
        "SELECT id FROM messages
         WHERE channel_id = ? AND deleted_at IS NULL
         ORDER BY id DESC
         LIMIT 1",
    )
    .bind(channel_id)
    .fetch_optional(&mut *conn)
    .await)
}

/// Newest `limit` visible messages older than `before`, in ascending id order.
///
/// `before = None` pages from the newest message. `limit = 0` is empty.
pub async fn page_before(
    conn: &mut SqliteConnection,
    channel_id: Id,
    before: Option<Id>,
    limit: u32,
) -> Result<Vec<Message>, Error> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut rows = if let Some(before) = before {
        older_than(conn, channel_id, before, limit).await?
    } else {
        newest(conn, channel_id, limit).await?
    };
    rows.reverse();
    Ok(rows)
}

/// Up to `limit` visible messages around `anchor`, in ascending id order.
///
/// See the module docs for which rows are chosen. `limit = 0` is empty.
pub async fn page_around(
    conn: &mut SqliteConnection,
    channel_id: Id,
    anchor: Id,
    limit: u32,
) -> Result<Vec<Message>, Error> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    if let Some(message) = visible(conn, channel_id, anchor).await? {
        return around_present(conn, channel_id, message, limit).await;
    }
    nearest(conn, channel_id, anchor, limit).await
}

async fn replace_mentions(
    conn: &mut SqliteConnection,
    message_id: Id,
    mentions: &[Id],
) -> Result<(), Error> {
    db(
        sqlx::query("DELETE FROM message_mentions WHERE message_id = ?")
            .bind(message_id)
            .execute(&mut *conn)
            .await,
    )?;
    for user_id in mentions {
        db(sqlx::query(
            "INSERT INTO message_mentions (message_id, user_id) VALUES (?, ?)
             ON CONFLICT (message_id, user_id) DO NOTHING",
        )
        .bind(message_id)
        .bind(*user_id)
        .execute(&mut *conn)
        .await)?;
    }
    Ok(())
}

async fn visible(
    conn: &mut SqliteConnection,
    channel_id: Id,
    id: Id,
) -> Result<Option<Message>, Error> {
    Ok(get(conn, id)
        .await?
        .filter(|message| message.channel_id == channel_id && message.deleted_at.is_none()))
}

async fn newest(
    conn: &mut SqliteConnection,
    channel_id: Id,
    limit: u32,
) -> Result<Vec<Message>, Error> {
    rows(conn, NEWEST, channel_id, i64::from(limit)).await
}

async fn older_than(
    conn: &mut SqliteConnection,
    channel_id: Id,
    id: Id,
    limit: u32,
) -> Result<Vec<Message>, Error> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let found = db(sqlx::query_as::<_, MessageRow>(OLDER)
        .bind(channel_id)
        .bind(id)
        .bind(i64::from(limit))
        .fetch_all(&mut *conn)
        .await)?;
    Ok(found.into_iter().map(MessageRow::into_message).collect())
}

async fn newer_than(
    conn: &mut SqliteConnection,
    channel_id: Id,
    id: Id,
    limit: u32,
) -> Result<Vec<Message>, Error> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let found = db(sqlx::query_as::<_, MessageRow>(NEWER)
        .bind(channel_id)
        .bind(id)
        .bind(i64::from(limit))
        .fetch_all(&mut *conn)
        .await)?;
    Ok(found.into_iter().map(MessageRow::into_message).collect())
}

async fn rows(
    conn: &mut SqliteConnection,
    sql: &'static str,
    channel_id: Id,
    limit: i64,
) -> Result<Vec<Message>, Error> {
    let found = db(sqlx::query_as::<_, MessageRow>(sql)
        .bind(channel_id)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await)?;
    Ok(found.into_iter().map(MessageRow::into_message).collect())
}

async fn around_present(
    conn: &mut SqliteConnection,
    channel_id: Id,
    anchor: Message,
    limit: u32,
) -> Result<Vec<Message>, Error> {
    let older_slots = (limit - 1) / 2;
    let newer_slots = limit - 1 - older_slots;
    let mut older = older_than(conn, channel_id, anchor.id, older_slots).await?;
    older.reverse();
    let mut newer = newer_than(conn, channel_id, anchor.id, newer_slots).await?;
    if len_u32(older.len()) < older_slots {
        let extra = older_slots - len_u32(older.len());
        newer = newer_than(conn, channel_id, anchor.id, newer_slots + extra).await?;
    } else if len_u32(newer.len()) < newer_slots {
        let extra = newer_slots - len_u32(newer.len());
        older = older_than(conn, channel_id, anchor.id, older_slots + extra).await?;
        older.reverse();
    }
    older.push(anchor);
    older.extend(newer);
    Ok(older)
}

async fn nearest(
    conn: &mut SqliteConnection,
    channel_id: Id,
    anchor: Id,
    limit: u32,
) -> Result<Vec<Message>, Error> {
    let older = older_than(conn, channel_id, anchor, limit).await?;
    let newer = newer_than(conn, channel_id, anchor, limit).await?;
    let mut older: VecDeque<Message> = older.into_iter().rev().collect();
    let mut newer: VecDeque<Message> = newer.into_iter().collect();
    let mut picked = Vec::new();
    while picked.len() < usize::try_from(limit).unwrap_or(usize::MAX) {
        let take_older = match (
            older.back().map(|message| message.id),
            newer.front().map(|message| message.id),
        ) {
            (None, None) => break,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some(left), Some(right)) => id_distance(anchor, left) <= id_distance(anchor, right),
        };
        let Some(message) = (if take_older {
            older.pop_back()
        } else {
            newer.pop_front()
        }) else {
            break;
        };
        picked.push(message);
    }
    picked.sort_by_key(|message| message.id);
    Ok(picked)
}

fn len_u32(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

const fn id_distance(left: Id, right: Id) -> u128 {
    u128::from_be_bytes(left.as_bytes()).abs_diff(u128::from_be_bytes(right.as_bytes()))
}

#[derive(Debug, sqlx::FromRow)]
struct MessageRow {
    id: Id,
    channel_id: Id,
    author_id: Option<Id>,
    webhook_id: Option<Id>,
    webhook_name: Option<String>,
    content: String,
    reply_to: Option<Id>,
    mentions_everyone: i64,
    created_at: i64,
    edited_at: Option<i64>,
    deleted_at: Option<i64>,
}

impl MessageRow {
    fn into_message(self) -> Message {
        Message {
            id: self.id,
            channel_id: self.channel_id,
            author_id: self.author_id,
            webhook_id: self.webhook_id,
            webhook_name: self.webhook_name,
            content: self.content,
            reply_to: self.reply_to,
            mentions_everyone: self.mentions_everyone != 0,
            created_at: stamp(self.created_at),
            edited_at: stamp_opt(self.edited_at),
            deleted_at: stamp_opt(self.deleted_at),
        }
    }
}

#[cfg(test)]
mod tests {
    use ss_core::Id;

    use super::{NewMessage, edit, get, insert, mentions, page_around, page_before, soft_delete};
    use crate::db::{Error, testing};

    fn message(id: Id, channel: Id, author: Id, content: &str) -> NewMessage {
        NewMessage {
            id,
            channel_id: channel,
            author_id: Some(author),
            webhook_id: None,
            webhook_name: None,
            content: content.into(),
            reply_to: None,
            mentions: Vec::new(),
            mentions_everyone: false,
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines, reason = "the body is the pagination table")]
    async fn pages_edits_and_soft_deletes() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let author = testing::user(&mut conn, 1).await;
        let other = testing::user(&mut conn, 2).await;
        let channel = testing::text_channel(&mut conn, 3, author.id).await;
        let ids: Vec<Id> = [10, 20, 30, 40, 50].into_iter().map(testing::id).collect();
        for (index, id) in ids.iter().copied().enumerate() {
            let mut draft = message(id, channel.id, author.id, &index.to_string());
            if index == 4 {
                draft.reply_to = Some(ids[0]);
                draft.mentions = vec![other.id, other.id];
            }
            insert(&mut conn, draft).await.unwrap();
        }
        assert_eq!(mentions(&mut conn, ids[4]).await.unwrap(), vec![other.id]);
        let edited_at = ss_core::Timestamp::from_millis(8);
        edit(&mut conn, ids[4], "changed", &[], false, edited_at)
            .await
            .unwrap();
        assert_eq!(mentions(&mut conn, ids[4]).await.unwrap().len(), 0);
        assert_eq!(
            get(&mut conn, ids[4]).await.unwrap().unwrap().edited_at,
            Some(edited_at)
        );

        let newest = page_before(&mut conn, channel.id, None, 2).await.unwrap();
        assert_eq!(ids_of(&newest), vec![ids[3], ids[4]]);
        let before = page_before(&mut conn, channel.id, Some(ids[3]), 2)
            .await
            .unwrap();
        assert_eq!(ids_of(&before), vec![ids[1], ids[2]]);
        assert_eq!(
            page_before(&mut conn, channel.id, Some(ids[0]), 2)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            page_before(&mut conn, channel.id, None, 0)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            page_before(&mut conn, channel.id, None, 100)
                .await
                .unwrap()
                .len(),
            5
        );

        assert_eq!(
            ids_of(&page_around(&mut conn, channel.id, ids[2], 3).await.unwrap()),
            vec![ids[1], ids[2], ids[3]]
        );
        assert_eq!(
            ids_of(&page_around(&mut conn, channel.id, ids[0], 3).await.unwrap()),
            vec![ids[0], ids[1], ids[2]]
        );
        assert_eq!(
            ids_of(&page_around(&mut conn, channel.id, ids[4], 3).await.unwrap()),
            vec![ids[2], ids[3], ids[4]]
        );
        assert_eq!(
            ids_of(&page_around(&mut conn, channel.id, ids[2], 1).await.unwrap()),
            vec![ids[2]]
        );

        let between = testing::id(25);
        assert_eq!(
            ids_of(
                &page_around(&mut conn, channel.id, between, 2)
                    .await
                    .unwrap()
            ),
            vec![ids[1], ids[2]]
        );
        assert_eq!(
            ids_of(
                &page_around(&mut conn, channel.id, between, 1)
                    .await
                    .unwrap()
            ),
            vec![ids[1]]
        );

        soft_delete(&mut conn, ids[2], ss_core::Timestamp::from_millis(9))
            .await
            .unwrap();
        assert!(
            get(&mut conn, ids[2])
                .await
                .unwrap()
                .unwrap()
                .deleted_at
                .is_some()
        );
        let after_delete = page_around(&mut conn, channel.id, ids[2], 3).await.unwrap();
        assert!(!ids_of(&after_delete).contains(&ids[2]));
        assert!(matches!(
            insert(&mut conn, message(ids[0], channel.id, author.id, "again")).await,
            Err(Error::Conflict)
        ));
        assert!(matches!(
            insert(
                &mut conn,
                message(testing::id(60), testing::id(99), author.id, "missing"),
            )
            .await,
            Err(Error::ForeignKey)
        ));
    }

    fn ids_of(messages: &[super::Message]) -> Vec<Id> {
        messages.iter().map(|message| message.id).collect()
    }
}
