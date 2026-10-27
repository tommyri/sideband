//! `SendMessage`, `EditMessage`, `DeleteMessage`, `Typing`, and `MarkRead`.
//!
//! The server stores raw text. Mentions are `<@ULID>` plus an authorized
//! `@everyone`. History pages live on the HTTP routes.

use std::{num::NonZeroU32, sync::Arc, time::Duration};

use governor::{Quota, RateLimiter, clock::DefaultClock, state::keyed::DefaultKeyedStateStore};
use ss_core::{ChannelKind, Id, Permissions, Timestamp};
use ss_proto::{
    v1::{self, ack, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};

use crate::{
    db::{self, NewMessage, attachments, channels, link_previews, messages, read_states, users},
    events::EventBus,
    perms::{AccessError, PermissionService},
    previews::{PreviewJob, PreviewQueue},
};

use super::{CommandReply, Error};

const MAX_CHARS: usize = 4000;
const MAX_NONCE: usize = 128;
const MAX_MENTIONS: usize = 50;
const EDIT_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const SEND_BURST: u32 = 5;
const EVERYONE: &str = "@everyone";

type IdLimiter = RateLimiter<Id, DefaultKeyedStateStore<Id>, DefaultClock>;
type TypingLimiter = RateLimiter<(Id, Id), DefaultKeyedStateStore<(Id, Id)>, DefaultClock>;

/// Text commands for one process.
pub struct Messages {
    perms: Arc<PermissionService>,
    sends: IdLimiter,
    typing: TypingLimiter,
    previews: PreviewQueue,
}

impl Messages {
    /// Five sends per five seconds, and one typing event per five seconds.
    #[must_use]
    pub fn new(perms: Arc<PermissionService>, pool: Arc<db::Pool>, bus: EventBus) -> Self {
        let send_quota = Quota::with_period(Duration::from_secs(5))
            .expect("five seconds")
            .allow_burst(NonZeroU32::new(SEND_BURST).expect("burst"));
        let typing_quota = Quota::with_period(Duration::from_secs(5)).expect("five seconds");
        Self {
            perms,
            sends: RateLimiter::keyed(send_quota),
            typing: RateLimiter::keyed(typing_quota),
            previews: PreviewQueue::new(pool, bus),
        }
    }

    /// Dispatches a text command. Other messages return `None`.
    pub async fn dispatch(
        &self,
        actor: Id,
        msg: &v1::client_envelope::Msg,
    ) -> Option<Result<CommandReply, Error>> {
        match msg {
            v1::client_envelope::Msg::SendMessage(command) => Some(self.send(actor, command).await),
            v1::client_envelope::Msg::EditMessage(command) => Some(self.edit(actor, command).await),
            v1::client_envelope::Msg::DeleteMessage(command) => {
                Some(self.delete(actor, command).await)
            }
            v1::client_envelope::Msg::Typing(command) => Some(self.typing(actor, command).await),
            v1::client_envelope::Msg::MarkRead(command) => {
                Some(self.mark_read(actor, command).await)
            }
            _ => None,
        }
    }

    async fn send(&self, actor: Id, command: &v1::SendMessage) -> Result<CommandReply, Error> {
        let channel_id = required_id(&command.channel_id, "channel id must be 16 bytes")?;
        let content = body(&command.content)?;
        let nonce = nonce(command.nonce.as_deref())?;
        let (mentioned, everyone) = parse_mentions(&content)?;
        self.allow(
            actor,
            channel_id,
            Permissions::VIEW_CHANNEL,
            "unknown channel",
        )?;
        self.allow(
            actor,
            channel_id,
            Permissions::SEND_MESSAGES,
            "missing send messages",
        )?;
        if !command.attachment_ids.is_empty() {
            self.allow(
                actor,
                channel_id,
                Permissions::ATTACH_FILES,
                "missing attach files",
            )?;
        }
        if everyone {
            self.allow(
                actor,
                channel_id,
                Permissions::MENTION_EVERYONE,
                "missing mention everyone",
            )?;
        }
        let mut conn = self.perms.pool().write().await;
        require_text(&mut conn, channel_id).await?;
        let reply_to = reply_target(&mut conn, channel_id, command.reply_to.as_deref()).await?;
        let mentions = known_users(&mut conn, &mentioned).await?;
        let files = claimable_files(&mut conn, actor, &command.attachment_ids).await?;
        if self.sends.check_key(&actor).is_err() {
            return Err(Error::RateLimited("sending messages too quickly"));
        }
        let id = Id::generate();
        let stored = messages::insert(
            &mut conn,
            NewMessage {
                id,
                channel_id,
                author_id: Some(actor),
                webhook_id: None,
                content: content.clone(),
                reply_to,
                mentions,
                mentions_everyone: everyone,
            },
        )
        .await?;
        for file in &files {
            attachments::set_message(&mut conn, file.id, Some(id)).await?;
        }
        let wire = to_proto(&mut conn, &stored).await?;
        drop(conn);
        self.previews.enqueue(PreviewJob {
            message_id: id,
            content,
        });
        Ok(CommandReply {
            ack: ack::Result::MessageId(id_to_bytes(id)),
            events: vec![server_envelope::Msg::MessageCreated(v1::MessageCreated {
                message: Some(wire),
                nonce,
            })],
        })
    }

    async fn edit(&self, actor: Id, command: &v1::EditMessage) -> Result<CommandReply, Error> {
        let message_id = required_id(&command.message_id, "message id must be 16 bytes")?;
        let content = body(&command.content)?;
        let (mentioned, everyone) = parse_mentions(&content)?;
        let mut conn = self.perms.pool().write().await;
        let current = live_message(&mut conn, message_id).await?;
        self.allow(
            actor,
            current.channel_id,
            Permissions::VIEW_CHANNEL,
            "unknown message",
        )?;
        if current.author_id != Some(actor) {
            return Err(Error::Forbidden("only the author can edit this message"));
        }
        if Timestamp::now()
            .as_millis()
            .saturating_sub(current.created_at.as_millis())
            > EDIT_WINDOW_MS
        {
            return Err(Error::Forbidden("edit window has closed"));
        }
        if everyone {
            self.allow(
                actor,
                current.channel_id,
                Permissions::MENTION_EVERYONE,
                "missing mention everyone",
            )?;
        }
        let mentions = known_users(&mut conn, &mentioned).await?;
        let stored = messages::edit(
            &mut conn,
            message_id,
            &content,
            &mentions,
            everyone,
            Timestamp::now(),
        )
        .await?;
        let wire = to_proto(&mut conn, &stored).await?;
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            events: vec![server_envelope::Msg::MessageUpdated(v1::MessageUpdated {
                message: Some(wire),
                nonce: None,
            })],
        })
    }

    async fn delete(&self, actor: Id, command: &v1::DeleteMessage) -> Result<CommandReply, Error> {
        let message_id = required_id(&command.message_id, "message id must be 16 bytes")?;
        let mut conn = self.perms.pool().write().await;
        let current = live_message(&mut conn, message_id).await?;
        self.allow(
            actor,
            current.channel_id,
            Permissions::VIEW_CHANNEL,
            "unknown message",
        )?;
        if current.author_id != Some(actor) {
            self.allow(
                actor,
                current.channel_id,
                Permissions::MANAGE_MESSAGES,
                "missing manage messages",
            )?;
        }
        let wire = to_proto(&mut conn, &current).await?;
        messages::soft_delete(&mut conn, message_id, Timestamp::now()).await?;
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            events: vec![server_envelope::Msg::MessageDeleted(v1::MessageDeleted {
                message: Some(wire),
                nonce: None,
            })],
        })
    }

    async fn typing(&self, actor: Id, command: &v1::Typing) -> Result<CommandReply, Error> {
        let channel_id = required_id(&command.channel_id, "channel id must be 16 bytes")?;
        self.allow(
            actor,
            channel_id,
            Permissions::VIEW_CHANNEL,
            "unknown channel",
        )?;
        self.allow(
            actor,
            channel_id,
            Permissions::SEND_MESSAGES,
            "missing send messages",
        )?;
        let mut conn = self.perms.pool().read().await?;
        require_text(&mut conn, channel_id).await?;
        drop(conn);
        if self.typing.check_key(&(actor, channel_id)).is_err() {
            return Err(Error::RateLimited("typing is rate limited"));
        }
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            events: vec![server_envelope::Msg::TypingStarted(v1::TypingStarted {
                channel_id: id_to_bytes(channel_id),
                user_id: id_to_bytes(actor),
            })],
        })
    }

    async fn mark_read(&self, actor: Id, command: &v1::MarkRead) -> Result<CommandReply, Error> {
        let channel_id = required_id(&command.channel_id, "channel id must be 16 bytes")?;
        let message_id = required_id(&command.message_id, "message id must be 16 bytes")?;
        self.allow(
            actor,
            channel_id,
            Permissions::VIEW_CHANNEL,
            "unknown channel",
        )?;
        let mut conn = self.perms.pool().write().await;
        let message = messages::get(&mut conn, message_id)
            .await?
            .ok_or(Error::NotFound("unknown message"))?;
        if message.channel_id != channel_id {
            return Err(Error::Invalid("message is in another channel"));
        }
        read_states::upsert(&mut conn, actor, channel_id, message_id).await?;
        let mention_count =
            messages::unread_mentions(&mut conn, channel_id, actor, message_id).await?;
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            events: vec![server_envelope::Msg::ReadStateUpdated(
                v1::ReadStateUpdated {
                    channel_id: id_to_bytes(channel_id),
                    last_read_message_id: id_to_bytes(message_id),
                    mention_count,
                },
            )],
        })
    }

    fn allow(
        &self,
        user: Id,
        channel: Id,
        permission: Permissions,
        message: &'static str,
    ) -> Result<(), Error> {
        self.perms
            .check(user, Some(channel), permission)
            .map_err(|error| match error {
                AccessError::NotFound => Error::NotFound(message),
                AccessError::Forbidden => Error::Forbidden(message),
            })
    }
}

/// Protobuf form of a stored message, including mentions and attachments.
pub(crate) async fn to_proto(
    conn: &mut sqlx::SqliteConnection,
    message: &db::Message,
) -> Result<v1::Message, db::Error> {
    let author = match message.author_id {
        Some(author_id) => users::get(conn, author_id).await?.map(|user| v1::User {
            id: id_to_bytes(user.id),
            username: user.username,
            display_name: user.display_name,
            avatar_attachment_id: user.avatar_attachment_id.map(id_to_bytes),
            kind: i32::from(match user.kind {
                ss_core::UserKind::Bot => v1::UserKind::Bot,
                _ => v1::UserKind::User,
            }),
        }),
        None => None,
    };
    let mentions = messages::mentions(conn, message.id).await?;
    let files = attachments::list_for_message(conn, message.id).await?;
    let previews = link_previews::ready_for_message(conn, message.id).await?;
    Ok(v1::Message {
        id: id_to_bytes(message.id),
        channel_id: id_to_bytes(message.channel_id),
        author,
        content: message.content.clone(),
        reply_to: message.reply_to.map(id_to_bytes),
        attachments: files.into_iter().map(wire_attachment).collect(),
        mentions: mentions.into_iter().map(id_to_bytes).collect(),
        link_previews: previews.into_iter().map(wire_preview).collect(),
        created_at: message.created_at.as_millis(),
        edited_at: message.edited_at.map(Timestamp::as_millis),
        webhook_id: message.webhook_id.map(id_to_bytes),
    })
}

fn wire_preview(preview: db::LinkPreview) -> v1::LinkPreview {
    v1::LinkPreview {
        url: preview.url,
        title: preview.title,
        description: preview.description,
        image_url: preview.image_url,
        site_name: preview.site_name,
    }
}

fn wire_attachment(file: db::Attachment) -> v1::Attachment {
    v1::Attachment {
        id: id_to_bytes(file.id),
        filename: file.filename,
        mime: file.mime,
        size: u64::try_from(file.size).unwrap_or(0),
        width: file.width.and_then(|value| u32::try_from(value).ok()),
        height: file.height.and_then(|value| u32::try_from(value).ok()),
    }
}

async fn require_text(conn: &mut sqlx::SqliteConnection, id: Id) -> Result<(), Error> {
    let channel = channels::get(conn, id)
        .await?
        .filter(|channel| channel.deleted_at.is_none())
        .ok_or(Error::NotFound("unknown channel"))?;
    if matches!(
        channel.kind,
        ChannelKind::Text | ChannelKind::Dm | ChannelKind::GroupDm
    ) {
        Ok(())
    } else {
        Err(Error::Invalid("not a text channel"))
    }
}

async fn live_message(conn: &mut sqlx::SqliteConnection, id: Id) -> Result<db::Message, Error> {
    messages::get(conn, id)
        .await?
        .filter(|message| message.deleted_at.is_none())
        .ok_or(Error::NotFound("unknown message"))
}

async fn reply_target(
    conn: &mut sqlx::SqliteConnection,
    channel_id: Id,
    reply_to: Option<&[u8]>,
) -> Result<Option<Id>, Error> {
    let Some(bytes) = reply_to else {
        return Ok(None);
    };
    let id = required_id(bytes, "reply id must be 16 bytes")?;
    let message = messages::get(conn, id)
        .await?
        .ok_or(Error::NotFound("unknown message"))?;
    if message.channel_id != channel_id {
        return Err(Error::Invalid("reply is in another channel"));
    }
    if message.deleted_at.is_some() {
        return Err(Error::Invalid("cannot reply to a deleted message"));
    }
    Ok(Some(id))
}

async fn known_users(conn: &mut sqlx::SqliteConnection, ids: &[Id]) -> Result<Vec<Id>, Error> {
    let mut kept = Vec::new();
    for id in ids {
        if users::get(conn, *id).await?.is_some() {
            kept.push(*id);
        }
    }
    Ok(kept)
}

async fn claimable_files(
    conn: &mut sqlx::SqliteConnection,
    actor: Id,
    ids: &[bytes::Bytes],
) -> Result<Vec<db::Attachment>, Error> {
    let mut files = Vec::new();
    for raw in ids {
        let id = required_id(raw, "attachment id must be 16 bytes")?;
        let file = attachments::get(conn, id)
            .await?
            .ok_or(Error::NotFound("unknown attachment"))?;
        if file.uploader_id != actor {
            return Err(Error::Forbidden("attachment belongs to someone else"));
        }
        if file.message_id.is_some() {
            return Err(Error::Invalid("attachment is already used"));
        }
        files.push(file);
    }
    Ok(files)
}

fn body(content: &str) -> Result<String, Error> {
    let trimmed = content.trim();
    let len = trimmed.chars().count();
    if len == 0 {
        return Err(Error::Invalid("message is empty"));
    }
    if len > MAX_CHARS {
        return Err(Error::Invalid("message is too long"));
    }
    Ok(trimmed.to_owned())
}

fn nonce(value: Option<&str>) -> Result<Option<String>, Error> {
    match value {
        None | Some("") => Ok(None),
        Some(value) if value.chars().count() > MAX_NONCE => {
            Err(Error::Invalid("nonce is too long"))
        }
        Some(value) => Ok(Some(value.to_owned())),
    }
}

fn parse_mentions(content: &str) -> Result<(Vec<Id>, bool), Error> {
    let mut users = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("<@") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('>') else {
            break;
        };
        if let Ok(id) = after[..end].parse::<Id>()
            && !users.contains(&id)
        {
            users.push(id);
        }
        rest = &after[end + 1..];
    }
    if users.len() > MAX_MENTIONS {
        return Err(Error::Invalid("too many mentions"));
    }
    users.sort_unstable();
    Ok((users, mentions_everyone(content)))
}

fn mentions_everyone(content: &str) -> bool {
    content.match_indices(EVERYONE).any(|(index, _)| {
        let before = index == 0
            || content[..index]
                .chars()
                .next_back()
                .is_none_or(|char| !char.is_ascii_alphanumeric());
        let after = index + EVERYONE.len();
        let after_ok = content[after..]
            .chars()
            .next()
            .is_none_or(|char| !char.is_ascii_alphanumeric());
        before && after_ok
    })
}

fn required_id(bytes: impl AsRef<[u8]>, message: &'static str) -> Result<Id, Error> {
    id_from_bytes(bytes.as_ref()).map_err(|_| Error::Invalid(message))
}
