//! `channels`, `channel_members`, and `channel_overrides` rows.

use sqlx::SqliteConnection;
use ss_core::{ChannelKind, Id, Permissions, Timestamp, VoiceProfile};

use super::{
    Error,
    convert::{
        changed, permissions_from_i64, permissions_to_i64, profile_from_text, stamp, stamp_opt,
    },
    db,
    model::{Channel, ChannelOverride},
};

/// Fields stored on insert. The repository sets `created_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChannel {
    /// Primary key.
    pub id: Id,
    /// Channel kind.
    pub kind: ChannelKind,
    /// Display name.
    pub name: String,
    /// Optional topic.
    pub topic: Option<String>,
    /// Parent category.
    pub parent_id: Option<Id>,
    /// Sibling order.
    pub position: i64,
    /// Voice profile.
    pub voice_profile: Option<VoiceProfile>,
    /// Voice bitrate in bits per second.
    pub voice_bitrate: Option<i64>,
    /// Voice user cap.
    pub user_limit: Option<i64>,
    /// Creator.
    pub created_by: Id,
}

/// Mutable channel fields. Kind and creator stay as inserted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelUpdate {
    /// Channel kind. Kind changes are the caller's responsibility.
    pub kind: ChannelKind,
    /// Display name.
    pub name: String,
    /// Optional topic.
    pub topic: Option<String>,
    /// Parent category.
    pub parent_id: Option<Id>,
    /// Sibling order.
    pub position: i64,
    /// Voice profile.
    pub voice_profile: Option<VoiceProfile>,
    /// Voice bitrate in bits per second.
    pub voice_bitrate: Option<i64>,
    /// Voice user cap.
    pub user_limit: Option<i64>,
}

const BY_ID: &str = "SELECT id, kind, name, topic, parent_id, position, voice_profile, voice_bitrate, user_limit, created_by, created_at, deleted_at FROM channels WHERE id = ?";
const LIVE: &str = "SELECT id, kind, name, topic, parent_id, position, voice_profile, voice_bitrate, user_limit, created_by, created_at, deleted_at FROM channels WHERE deleted_at IS NULL ORDER BY position, id";

/// Inserts a channel.
pub async fn insert(conn: &mut SqliteConnection, channel: NewChannel) -> Result<Channel, Error> {
    let created_at = Timestamp::now().as_millis();
    db(sqlx::query(
        "INSERT INTO channels
         (id, kind, name, topic, parent_id, position, voice_profile, voice_bitrate, user_limit, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(channel.id)
    .bind(channel.kind.as_i64())
    .bind(&channel.name)
    .bind(&channel.topic)
    .bind(channel.parent_id)
    .bind(channel.position)
    .bind(channel.voice_profile.map(VoiceProfile::as_str))
    .bind(channel.voice_bitrate)
    .bind(channel.user_limit)
    .bind(channel.created_by)
    .bind(created_at)
    .execute(&mut *conn)
    .await)?;
    get(conn, channel.id).await?.ok_or(Error::NotFound)
}

/// Loads a channel by id, including soft-deleted rows.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<Channel>, Error> {
    let row = db(sqlx::query_as::<_, ChannelRow>(BY_ID)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await)?;
    row.map(ChannelRow::into_channel).transpose()
}

/// Replaces the mutable fields.
pub async fn update(
    conn: &mut SqliteConnection,
    id: Id,
    channel: ChannelUpdate,
) -> Result<Channel, Error> {
    changed(db(sqlx::query(
        "UPDATE channels
         SET kind = ?, name = ?, topic = ?, parent_id = ?, position = ?, voice_profile = ?, voice_bitrate = ?, user_limit = ?
         WHERE id = ?",
    )
    .bind(channel.kind.as_i64())
    .bind(&channel.name)
    .bind(&channel.topic)
    .bind(channel.parent_id)
    .bind(channel.position)
    .bind(channel.voice_profile.map(VoiceProfile::as_str))
    .bind(channel.voice_bitrate)
    .bind(channel.user_limit)
    .bind(id)
    .execute(&mut *conn)
    .await)?)?;
    get(conn, id).await?.ok_or(Error::NotFound)
}

/// Sets `deleted_at`.
pub async fn soft_delete(
    conn: &mut SqliteConnection,
    id: Id,
    deleted_at: Timestamp,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE channels SET deleted_at = ? WHERE id = ?",
    )
    .bind(deleted_at.as_millis())
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Adds a DM member. Adding the same member twice is a conflict.
pub async fn add_member(
    conn: &mut SqliteConnection,
    channel_id: Id,
    user_id: Id,
) -> Result<(), Error> {
    db(
        sqlx::query("INSERT INTO channel_members (channel_id, user_id) VALUES (?, ?)")
            .bind(channel_id)
            .bind(user_id)
            .execute(&mut *conn)
            .await,
    )?;
    Ok(())
}

/// Removes a DM member.
pub async fn remove_member(
    conn: &mut SqliteConnection,
    channel_id: Id,
    user_id: Id,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "DELETE FROM channel_members WHERE channel_id = ? AND user_id = ?",
    )
    .bind(channel_id)
    .bind(user_id)
    .execute(&mut *conn)
    .await)?)
}

/// Members of a channel, ordered by user id.
pub async fn members(conn: &mut SqliteConnection, channel_id: Id) -> Result<Vec<Id>, Error> {
    db(sqlx::query_scalar(
        "SELECT user_id FROM channel_members WHERE channel_id = ? ORDER BY user_id",
    )
    .bind(channel_id)
    .fetch_all(&mut *conn)
    .await)
}

/// Inserts or replaces the overwrite for one role.
pub async fn set_override(
    conn: &mut SqliteConnection,
    channel_id: Id,
    role_id: Id,
    allow: Permissions,
    deny: Permissions,
) -> Result<(), Error> {
    let allow = permissions_to_i64(allow)?;
    let deny = permissions_to_i64(deny)?;
    db(sqlx::query(
        "INSERT INTO channel_overrides (channel_id, role_id, allow, deny) VALUES (?, ?, ?, ?)
         ON CONFLICT (channel_id, role_id) DO UPDATE SET allow = excluded.allow, deny = excluded.deny",
    )
    .bind(channel_id)
    .bind(role_id)
    .bind(allow)
    .bind(deny)
    .execute(&mut *conn)
    .await)?;
    Ok(())
}

/// Removes an overwrite.
pub async fn delete_override(
    conn: &mut SqliteConnection,
    channel_id: Id,
    role_id: Id,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "DELETE FROM channel_overrides WHERE channel_id = ? AND role_id = ?",
    )
    .bind(channel_id)
    .bind(role_id)
    .execute(&mut *conn)
    .await)?)
}

/// Overwrites for a channel, ordered by role id.
pub async fn overrides(
    conn: &mut SqliteConnection,
    channel_id: Id,
) -> Result<Vec<ChannelOverride>, Error> {
    let rows = db(sqlx::query_as::<_, OverrideRow>(
        "SELECT channel_id, role_id, allow, deny FROM channel_overrides WHERE channel_id = ? ORDER BY role_id",
    )
    .bind(channel_id)
    .fetch_all(&mut *conn)
    .await)?;
    rows.into_iter().map(OverrideRow::into_override).collect()
}

/// Channels that have not been soft-deleted, in sibling order.
pub async fn list(conn: &mut SqliteConnection) -> Result<Vec<Channel>, Error> {
    let rows = db(sqlx::query_as::<_, ChannelRow>(LIVE)
        .fetch_all(&mut *conn)
        .await)?;
    rows.into_iter().map(ChannelRow::into_channel).collect()
}

/// Every channel membership.
pub async fn memberships(conn: &mut SqliteConnection) -> Result<Vec<(Id, Id)>, Error> {
    let rows = db(sqlx::query_as::<_, MemberRow>(
        "SELECT channel_id, user_id FROM channel_members",
    )
    .fetch_all(&mut *conn)
    .await)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.channel_id, row.user_id))
        .collect())
}

/// Every channel overwrite.
pub async fn list_overrides(conn: &mut SqliteConnection) -> Result<Vec<ChannelOverride>, Error> {
    let rows = db(sqlx::query_as::<_, OverrideRow>(
        "SELECT channel_id, role_id, allow, deny FROM channel_overrides",
    )
    .fetch_all(&mut *conn)
    .await)?;
    rows.into_iter().map(OverrideRow::into_override).collect()
}

/// Sets the parent and sibling position. Zero rows is not found.
pub async fn place(
    conn: &mut SqliteConnection,
    id: Id,
    parent_id: Option<Id>,
    position: i64,
) -> Result<(), Error> {
    changed(db(sqlx::query(
        "UPDATE channels SET parent_id = ?, position = ? WHERE id = ?",
    )
    .bind(parent_id)
    .bind(position)
    .bind(id)
    .execute(&mut *conn)
    .await)?)
}

/// Guild channels ever created, including soft-deleted rows. DMs are excluded.
pub async fn guild_count(conn: &mut SqliteConnection) -> Result<i64, Error> {
    db(
        sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE kind NOT IN (3, 4)")
            .fetch_one(&mut *conn)
            .await,
    )
}

/// Removes every overwrite for `role_id`. Zero rows is success.
///
/// Role deletion fails the foreign key while an overwrite remains.
pub async fn clear_role_overrides(conn: &mut SqliteConnection, role_id: Id) -> Result<(), Error> {
    db(
        sqlx::query("DELETE FROM channel_overrides WHERE role_id = ?")
            .bind(role_id)
            .execute(&mut *conn)
            .await,
    )?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct MemberRow {
    channel_id: Id,
    user_id: Id,
}

#[derive(Debug, sqlx::FromRow)]
struct ChannelRow {
    id: Id,
    kind: i64,
    name: String,
    topic: Option<String>,
    parent_id: Option<Id>,
    position: i64,
    voice_profile: Option<String>,
    voice_bitrate: Option<i64>,
    user_limit: Option<i64>,
    created_by: Id,
    created_at: i64,
    deleted_at: Option<i64>,
}

impl ChannelRow {
    fn into_channel(self) -> Result<Channel, Error> {
        Ok(Channel {
            id: self.id,
            kind: ChannelKind::try_from(self.kind)?,
            name: self.name,
            topic: self.topic,
            parent_id: self.parent_id,
            position: self.position,
            voice_profile: profile_from_text(self.voice_profile)?,
            voice_bitrate: self.voice_bitrate,
            user_limit: self.user_limit,
            created_by: self.created_by,
            created_at: stamp(self.created_at),
            deleted_at: stamp_opt(self.deleted_at),
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct OverrideRow {
    channel_id: Id,
    role_id: Id,
    allow: i64,
    deny: i64,
}

impl OverrideRow {
    fn into_override(self) -> Result<ChannelOverride, Error> {
        Ok(ChannelOverride {
            channel_id: self.channel_id,
            role_id: self.role_id,
            allow: permissions_from_i64(self.allow)?,
            deny: permissions_from_i64(self.deny)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use ss_core::{ChannelKind, Permissions, VoiceProfile};

    use super::{
        ChannelUpdate, NewChannel, add_member, delete_override, get, insert, members, overrides,
        remove_member, set_override, soft_delete, update,
    };
    use crate::db::{roles::EVERYONE_ROLE_ID, testing};

    #[tokio::test]
    async fn insert_update_members_overrides_and_soft_delete() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let owner = testing::user(&mut conn, 1).await;
        let created = insert(
            &mut conn,
            NewChannel {
                id: testing::id(2),
                kind: ChannelKind::Voice,
                name: "Lounge".into(),
                topic: None,
                parent_id: None,
                position: 0,
                voice_profile: Some(VoiceProfile::Music),
                voice_bitrate: Some(128_000),
                user_limit: Some(8),
                created_by: owner.id,
            },
        )
        .await
        .unwrap();
        assert_eq!(created.voice_profile, Some(VoiceProfile::Music));
        let renamed = update(
            &mut conn,
            created.id,
            ChannelUpdate {
                kind: ChannelKind::Voice,
                name: "Music".into(),
                topic: Some("listening".into()),
                parent_id: None,
                position: 1,
                voice_profile: Some(VoiceProfile::Music),
                voice_bitrate: Some(128_000),
                user_limit: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(renamed.topic.as_deref(), Some("listening"));
        add_member(&mut conn, created.id, owner.id).await.unwrap();
        assert_eq!(
            members(&mut conn, created.id).await.unwrap(),
            vec![owner.id]
        );
        remove_member(&mut conn, created.id, owner.id)
            .await
            .unwrap();
        set_override(
            &mut conn,
            created.id,
            EVERYONE_ROLE_ID,
            Permissions::VIEW_CHANNEL,
            Permissions::SPEAK,
        )
        .await
        .unwrap();
        set_override(
            &mut conn,
            created.id,
            EVERYONE_ROLE_ID,
            Permissions::CONNECT,
            Permissions::empty(),
        )
        .await
        .unwrap();
        let rows = overrides(&mut conn, created.id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].allow, Permissions::CONNECT);
        assert_eq!(rows[0].deny, Permissions::empty());
        delete_override(&mut conn, created.id, EVERYONE_ROLE_ID)
            .await
            .unwrap();
        let deleted_at = ss_core::Timestamp::from_millis(4);
        soft_delete(&mut conn, created.id, deleted_at)
            .await
            .unwrap();
        assert_eq!(
            get(&mut conn, created.id)
                .await
                .unwrap()
                .unwrap()
                .deleted_at,
            Some(deleted_at)
        );
    }
}
