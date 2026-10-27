//! Row types returned by the repositories.
//!
//! Kinds, ids, permissions, voice profiles, and timestamps are the `ss-core`
//! values stored in SQLite. Preview status codes are the `link_previews.status`
//! integers: pending `0`, ready `1`, failed `2`.

use ss_core::{ChannelKind, Id, Permissions, Timestamp, UserKind, VoiceProfile};

/// A user account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// Primary key.
    pub id: Id,
    /// Unique login name. Compared case-insensitively.
    pub username: String,
    /// Name shown in the client.
    pub display_name: String,
    /// Password hash. This crate does not hash or verify it.
    pub password_hash: String,
    /// Avatar file, when one is set.
    pub avatar_attachment_id: Option<Id>,
    /// Human or bot.
    pub kind: UserKind,
    /// When the row was inserted.
    pub created_at: Timestamp,
    /// When the account was disabled. `None` means it can log in.
    pub disabled_at: Option<Timestamp>,
}

/// A login session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Primary key.
    pub id: Id,
    /// Account this session belongs to.
    pub user_id: Id,
    /// Hash of the bearer token. The token itself is not stored.
    pub token_hash: String,
    /// Client-supplied device label.
    pub device_name: String,
    /// When the session was created.
    pub created_at: Timestamp,
    /// Last time the session was used.
    pub last_seen_at: Timestamp,
    /// Optional expiry. `None` does not expire on its own.
    pub expires_at: Option<Timestamp>,
}

/// An invite code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// Primary key.
    pub id: Id,
    /// Unique code presented to the new user.
    pub code: String,
    /// User who created the invite.
    pub created_by: Id,
    /// Cap on [`Self::uses`]. `None` means no cap.
    pub max_uses: Option<i64>,
    /// How many times the code has been consumed.
    pub uses: i64,
    /// Optional expiry.
    pub expires_at: Option<Timestamp>,
    /// When the invite was revoked.
    pub revoked_at: Option<Timestamp>,
    /// When the invite was created.
    pub created_at: Timestamp,
}

/// A role. `@everyone` has [`Role::is_default`] set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    /// Primary key.
    pub id: Id,
    /// Display name, including `@everyone`.
    pub name: String,
    /// Optional RGB color packed by the caller.
    pub color: Option<i64>,
    /// Permission bits.
    pub permissions: Permissions,
    /// Higher positions outrank lower ones.
    pub position: i64,
    /// The seeded `@everyone` role.
    pub is_default: bool,
    /// When the role was created. Seeded roles use `0`.
    pub created_at: Timestamp,
}

/// A channel in the tree, including DMs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// Primary key.
    pub id: Id,
    /// Category, text, voice, DM, or group DM.
    pub kind: ChannelKind,
    /// Display name.
    pub name: String,
    /// Optional topic.
    pub topic: Option<String>,
    /// Parent category, when this channel is nested.
    pub parent_id: Option<Id>,
    /// Order among siblings.
    pub position: i64,
    /// Voice encoder profile. Only meaningful for voice channels.
    pub voice_profile: Option<VoiceProfile>,
    /// Optional voice bitrate override, in bits per second.
    pub voice_bitrate: Option<i64>,
    /// Optional voice user cap.
    pub user_limit: Option<i64>,
    /// User who created the channel.
    pub created_by: Id,
    /// When the channel was created.
    pub created_at: Timestamp,
    /// Soft-delete time.
    pub deleted_at: Option<Timestamp>,
}

/// Allow and deny masks for one role in one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelOverride {
    /// Channel the overwrite applies to.
    pub channel_id: Id,
    /// Role the overwrite applies to.
    pub role_id: Id,
    /// Bits added after the deny mask.
    pub allow: Permissions,
    /// Bits removed before the allow mask.
    pub deny: Permissions,
}

/// A text message. Deleted rows stay in the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Primary key. Also the pagination cursor.
    pub id: Id,
    /// Channel the message belongs to.
    pub channel_id: Id,
    /// Author. Empty for webhook-only messages.
    pub author_id: Option<Id>,
    /// Webhook that posted the message, when it was not a user.
    pub webhook_id: Option<Id>,
    /// Message body.
    pub content: String,
    /// Message this one replies to.
    pub reply_to: Option<Id>,
    /// The author was allowed to ping `@everyone`.
    pub mentions_everyone: bool,
    /// When the message was created.
    pub created_at: Timestamp,
    /// Last edit time.
    pub edited_at: Option<Timestamp>,
    /// Soft-delete time. Pages skip these rows.
    pub deleted_at: Option<Timestamp>,
}

/// A stored file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Primary key.
    pub id: Id,
    /// User who uploaded the file.
    pub uploader_id: Id,
    /// Message the file is attached to, once it is sent.
    pub message_id: Option<Id>,
    /// Original file name.
    pub filename: String,
    /// Media type.
    pub mime: String,
    /// Size in bytes.
    pub size: i64,
    /// SHA-256 of the bytes.
    pub sha256: [u8; 32],
    /// Image width, when known.
    pub width: Option<i64>,
    /// Image height, when known.
    pub height: Option<i64>,
    /// Path relative to the server data directory.
    pub storage_path: String,
    /// When the file was stored.
    pub created_at: Timestamp,
}

/// Fetch state of one URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum PreviewStatus {
    /// Not fetched yet.
    Pending = 0,
    /// Fetch succeeded.
    Ready = 1,
    /// Fetch failed.
    Failed = 2,
}

impl PreviewStatus {
    pub(crate) fn from_i64(value: i64) -> Result<Self, super::Error> {
        match value {
            0 => Ok(Self::Pending),
            1 => Ok(Self::Ready),
            2 => Ok(Self::Failed),
            _ => Err(super::Error::Corrupt("link preview status")),
        }
    }
}

/// Cached metadata for one URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreview {
    /// SHA-256 of the URL string.
    pub url_hash: [u8; 32],
    /// URL that was fetched.
    pub url: String,
    /// Fetch state.
    pub status: PreviewStatus,
    /// Page title.
    pub title: Option<String>,
    /// Page description.
    pub description: Option<String>,
    /// Preview image URL.
    pub image_url: Option<String>,
    /// Site name.
    pub site_name: Option<String>,
    /// When the preview was last fetched.
    pub fetched_at: Timestamp,
    /// When the cache row should be refreshed.
    pub expires_at: Timestamp,
}

/// One preview attached to a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageLink {
    /// Message that contains the URL.
    pub message_id: Id,
    /// Preview row.
    pub url_hash: [u8; 32],
    /// Order of the link inside the message.
    pub position: i64,
}

/// How far a user has read a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadState {
    /// User.
    pub user_id: Id,
    /// Channel.
    pub channel_id: Id,
    /// Newest message the user has read.
    pub last_read_message_id: Id,
}

/// A ban. A lifted row stays for history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ban {
    /// Primary key.
    pub id: Id,
    /// Banned user.
    pub user_id: Id,
    /// Moderator who banned them.
    pub banned_by: Id,
    /// Optional reason.
    pub reason: Option<String>,
    /// When the ban was created.
    pub created_at: Timestamp,
    /// Optional expiry.
    pub expires_at: Option<Timestamp>,
    /// When the ban was lifted.
    pub lifted_at: Option<Timestamp>,
}

/// One audit row. `details` is JSON text supplied by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Primary key.
    pub id: Id,
    /// Actor. Empty for system actions.
    pub actor_id: Option<Id>,
    /// Action name.
    pub action: String,
    /// Kind of the target, such as `user` or `channel`.
    pub target_kind: String,
    /// Target id, when the action has one.
    pub target_id: Option<Id>,
    /// JSON object with action-specific fields.
    pub details: String,
    /// When the action was recorded.
    pub created_at: Timestamp,
}

/// An incoming webhook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Webhook {
    /// Primary key.
    pub id: Id,
    /// Channel the webhook posts into.
    pub channel_id: Id,
    /// Display name.
    pub name: String,
    /// Hash of the webhook token.
    pub token_hash: String,
    /// User who created the webhook.
    pub created_by: Id,
    /// When the webhook was created.
    pub created_at: Timestamp,
    /// When the webhook was revoked.
    pub revoked_at: Option<Timestamp>,
}

/// One server setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    /// Setting name.
    pub key: String,
    /// Setting value.
    pub value: String,
}
