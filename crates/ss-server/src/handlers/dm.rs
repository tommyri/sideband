//! `CreateDm`, `LeaveDm`, `AddDmMembers`, `ListDms`, and group-DM renames.
//!
//! A 1:1 conversation is unique per unordered pair. A group conversation is
//! created fresh and holds the caller plus two to nine other people. Members
//! receive `VIEW|SEND|ATTACH` from [`ss_core::Permissions::resolve`]; roles
//! do not apply. Disabled and banned users cannot be added.
//!
//! Every DM a member is in goes into their `Welcome`, so group DMs are capped
//! per creator and per member ([`limits::GROUP_DMS_PER_CREATOR`],
//! [`limits::GROUP_DMS_PER_MEMBER`]) and new conversations have their own
//! quota. Mutations patch the permission snapshot with
//! [`PermissionService::set_direct`] instead of reloading it.

use sqlx::Connection;
use ss_core::{ChannelKind, Id, Timestamp};
use ss_proto::{
    v1::{self, ack, server_envelope},
    validate::id_from_bytes,
};
use std::sync::Arc;

use crate::{
    db::{
        self, ChannelUpdate, NewAuditEntry, NewChannel, audit_log, bans, channels, messages, users,
    },
    limits::{self, KeyedLimiter, quota},
    perms::PermissionService,
    snapshot,
};

use super::{CommandReply, Error};

const MAX_OTHERS: usize = 9;
const MAX_PEOPLE: usize = 10;
const MAX_NAME: usize = 64;

/// Direct-message commands. Mutations patch [`PermissionService`].
pub struct DirectMessages {
    perms: Arc<PermissionService>,
    creations: KeyedLimiter<Id>,
}

impl DirectMessages {
    /// Shares `perms` with the snapshot and the other handlers.
    #[must_use]
    pub fn new(perms: Arc<PermissionService>) -> Self {
        Self {
            perms,
            creations: KeyedLimiter::new(quota(limits::DM_CREATE_BURST, limits::DM_CREATE_PERIOD)),
        }
    }

    /// Dispatches a DM command. Guild `UpdateChannel` returns `None`.
    pub async fn dispatch(
        &self,
        actor: Id,
        msg: &v1::client_envelope::Msg,
    ) -> Option<Result<CommandReply, Error>> {
        match msg {
            v1::client_envelope::Msg::CreateDm(command) => Some(self.create(actor, command).await),
            v1::client_envelope::Msg::LeaveDm(command) => Some(self.leave(actor, command).await),
            v1::client_envelope::Msg::AddDmMembers(command) => Some(self.add(actor, command).await),
            v1::client_envelope::Msg::ListDms(command) => Some(self.list(actor, command).await),
            v1::client_envelope::Msg::UpdateChannel(command) => {
                self.rename_if_direct(actor, command).await
            }
            _ => None,
        }
    }

    /// Opens a 1:1 DM or creates a group DM.
    ///
    /// Reopening an existing 1:1 conversation is answered from a read
    /// connection and costs no quota. Anything that would insert a channel
    /// spends one cell of the creation quota first.
    async fn create(&self, actor: Id, command: &v1::CreateDm) -> Result<CommandReply, Error> {
        let people = participants(actor, &command.user_ids)?;
        if let [only] = people.as_slice() {
            let mut conn = self.perms.pool().read().await?;
            reachable(&mut conn, *only).await?;
            if let Some(existing) = channels::find_pair(&mut conn, actor, *only).await? {
                let members = channels::members(&mut conn, existing.id).await?;
                tracing::info!(%actor, channel_id = %existing.id, "dm reused");
                return conversation(&existing, &members, None, false);
            }
        }
        self.creations
            .check(&actor)
            .map_err(|wait| Error::RateLimited("creating conversations too quickly", wait))?;
        let mut guard = self.perms.pool().write().await;
        let mut tx = guard.begin().await.map_err(db::Error::from)?;
        for person in &people {
            reachable(&mut tx, *person).await?;
        }
        if let [only] = people.as_slice()
            && let Some(existing) = channels::find_pair(&mut tx, actor, *only).await?
        {
            let members = channels::members(&mut tx, existing.id).await?;
            tx.commit().await.map_err(db::Error::from)?;
            tracing::info!(%actor, channel_id = %existing.id, "dm reused");
            return conversation(&existing, &members, None, false);
        }
        if people.len() > 1 {
            if count(channels::groups_created(&mut tx, actor).await?)
                >= limits::GROUP_DMS_PER_CREATOR
            {
                return Err(Error::Forbidden(
                    "you have started too many group conversations",
                ));
            }
            let mut everyone = people.clone();
            everyone.push(actor);
            room_for(&mut tx, &everyone).await?;
        }
        let channel = insert_conversation(&mut tx, actor, &people).await?;
        let members = channels::members(&mut tx, channel.id).await?;
        tx.commit().await.map_err(db::Error::from)?;
        self.perms.set_direct(channel.id, channel.kind, &members);
        tracing::info!(%actor, channel_id = %channel.id, "dm created");
        conversation(&channel, &members, None, true)
    }

    /// A page of the caller's conversations, most recently active first.
    async fn list(&self, actor: Id, command: &v1::ListDms) -> Result<CommandReply, Error> {
        let before = command
            .cursor
            .as_deref()
            .map(|bytes| id_from_bytes(bytes).map_err(|_| Error::Invalid("unknown cursor")))
            .transpose()?;
        let limit = match usize::try_from(command.limit) {
            Ok(0) | Err(_) => limits::DM_PAGE,
            Ok(limit) => limit.min(limits::DM_PAGE),
        };
        let mut conn = self.perms.pool().read().await?;
        let page = snapshot::direct_page(&self.perms, &mut conn, actor, before, limit).await?;
        Ok(CommandReply {
            ack: ack::Result::DmPage(v1::DmPage {
                channels: page.channels,
                read_states: page.read_states,
                next: page.next,
            }),
            events: Vec::new(),
        })
    }

    /// Removes the caller from a group DM.
    async fn leave(&self, actor: Id, command: &v1::LeaveDm) -> Result<CommandReply, Error> {
        let id = required_id(&command.channel_id)?;
        let (channel, members, last) = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let (channel, _) = membership(&mut tx, id, actor).await?;
            if channel.kind != ChannelKind::GroupDm {
                return Err(Error::Invalid("a direct message cannot be left"));
            }
            channels::remove_member(&mut tx, id, actor).await?;
            audit(&mut tx, actor, "dm.leave", id).await?;
            let members = channels::members(&mut tx, id).await?;
            let last = messages::latest_id(&mut tx, id).await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.set_direct(id, channel.kind, &members);
            (channel, members, last)
        };
        tracing::info!(%actor, channel_id = %id, "dm left");
        updated(&channel, &members, last)
    }

    /// Adds people to a group DM. Only the creator may call this.
    async fn add(&self, actor: Id, command: &v1::AddDmMembers) -> Result<CommandReply, Error> {
        let id = required_id(&command.channel_id)?;
        let incoming = participants(actor, &command.user_ids)?;
        let (channel, members, last) = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let (channel, members) = membership(&mut tx, id, actor).await?;
            if channel.kind != ChannelKind::GroupDm {
                return Err(Error::Invalid("only a group conversation can gain members"));
            }
            if channel.created_by != actor {
                return Err(Error::Forbidden("only the creator can add members"));
            }
            for person in &incoming {
                if members.contains(person) {
                    return Err(Error::Invalid("already in the conversation"));
                }
                reachable(&mut tx, *person).await?;
            }
            if members.len().saturating_add(incoming.len()) > MAX_PEOPLE {
                return Err(Error::Invalid(
                    "a group conversation can include at most 9 other people",
                ));
            }
            room_for(&mut tx, &incoming).await?;
            for person in &incoming {
                channels::add_member(&mut tx, id, *person).await?;
            }
            audit(&mut tx, actor, "dm.add", id).await?;
            let members = channels::members(&mut tx, id).await?;
            let last = messages::latest_id(&mut tx, id).await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.set_direct(id, channel.kind, &members);
            (channel, members, last)
        };
        tracing::info!(%actor, channel_id = %id, "dm members added");
        updated(&channel, &members, last)
    }

    /// Claims `UpdateChannel` when the target is a live DM.
    async fn rename_if_direct(
        &self,
        actor: Id,
        command: &v1::UpdateChannel,
    ) -> Option<Result<CommandReply, Error>> {
        let id = required_id(&command.channel_id).ok()?;
        // The permission snapshot holds every live channel's kind.
        if self.perms.direct_members(id).is_some() {
            Some(self.rename(actor, id, command).await)
        } else {
            None
        }
    }

    /// Sets the name of a group DM. Any member may do this.
    async fn rename(
        &self,
        actor: Id,
        id: Id,
        command: &v1::UpdateChannel,
    ) -> Result<CommandReply, Error> {
        let (channel, members, last) = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let (current, _) = membership(&mut tx, id, actor).await?;
            if current.kind != ChannelKind::GroupDm {
                return Err(Error::Invalid("only a group conversation can be renamed"));
            }
            let name = group_name(command)?;
            let channel = channels::update(
                &mut tx,
                id,
                ChannelUpdate {
                    kind: current.kind,
                    name,
                    topic: current.topic.clone(),
                    parent_id: current.parent_id,
                    position: current.position,
                    voice_profile: current.voice_profile,
                    voice_bitrate: current.voice_bitrate,
                    user_limit: current.user_limit,
                },
            )
            .await?;
            audit(&mut tx, actor, "dm.rename", id).await?;
            let members = channels::members(&mut tx, id).await?;
            let last = messages::latest_id(&mut tx, id).await?;
            tx.commit().await.map_err(db::Error::from)?;
            (channel, members, last)
        };
        tracing::info!(%actor, channel_id = %id, "dm renamed");
        updated(&channel, &members, last)
    }
}

async fn insert_conversation(
    conn: &mut sqlx::SqliteConnection,
    actor: Id,
    people: &[Id],
) -> Result<db::Channel, Error> {
    let kind = if people.len() == 1 {
        ChannelKind::Dm
    } else {
        ChannelKind::GroupDm
    };
    let id = Id::generate();
    let channel = channels::insert(
        conn,
        NewChannel {
            id,
            kind,
            name: String::new(),
            topic: None,
            parent_id: None,
            position: 0,
            voice_profile: None,
            voice_bitrate: None,
            user_limit: None,
            created_by: actor,
        },
    )
    .await?;
    channels::add_member(conn, id, actor).await?;
    for person in people {
        channels::add_member(conn, id, *person).await?;
    }
    audit(conn, actor, "dm.create", id).await?;
    Ok(channel)
}

/// Fails when any of `people` is already in [`limits::GROUP_DMS_PER_MEMBER`]
/// group conversations.
async fn room_for(conn: &mut sqlx::SqliteConnection, people: &[Id]) -> Result<(), Error> {
    for person in people {
        if count(channels::groups_joined(conn, *person).await?) >= limits::GROUP_DMS_PER_MEMBER {
            return Err(Error::Forbidden("user is in too many group conversations"));
        }
    }
    Ok(())
}

fn count(rows: i64) -> usize {
    usize::try_from(rows).unwrap_or(usize::MAX)
}

async fn membership(
    conn: &mut sqlx::SqliteConnection,
    id: Id,
    actor: Id,
) -> Result<(db::Channel, Vec<Id>), Error> {
    let channel = channels::get(conn, id)
        .await?
        .ok_or(Error::NotFound("unknown channel"))?;
    if channel.deleted_at.is_some() || !channel.kind.is_direct() {
        return Err(Error::NotFound("unknown channel"));
    }
    let members = channels::members(conn, id).await?;
    if members.contains(&actor) {
        Ok((channel, members))
    } else {
        Err(Error::NotFound("unknown channel"))
    }
}

async fn reachable(conn: &mut sqlx::SqliteConnection, user_id: Id) -> Result<(), Error> {
    let user = users::get(conn, user_id)
        .await?
        .ok_or(Error::NotFound("unknown user"))?;
    if user.disabled_at.is_some() || banned(conn, user_id).await? {
        Err(Error::Forbidden("user cannot be messaged"))
    } else {
        Ok(())
    }
}

async fn banned(conn: &mut sqlx::SqliteConnection, user_id: Id) -> Result<bool, Error> {
    let now = Timestamp::now();
    let rows = bans::list_for_user(conn, user_id).await?;
    Ok(rows
        .into_iter()
        .any(|ban| ban.lifted_at.is_none() && ban.expires_at.is_none_or(|expires| expires > now)))
}

async fn audit(
    conn: &mut sqlx::SqliteConnection,
    actor: Id,
    action: &'static str,
    channel_id: Id,
) -> Result<(), Error> {
    audit_log::insert(
        conn,
        NewAuditEntry {
            id: Id::generate(),
            actor_id: Some(actor),
            action: action.to_owned(),
            target_kind: "channel".to_owned(),
            target_id: Some(channel_id),
            details: "{}".to_owned(),
        },
    )
    .await?;
    Ok(())
}

fn participants(actor: Id, ids: &[bytes::Bytes]) -> Result<Vec<Id>, Error> {
    let mut people = Vec::new();
    for bytes in ids {
        let id = required_id(bytes)?;
        if id != actor && !people.contains(&id) {
            people.push(id);
        }
    }
    if people.is_empty() {
        return Err(Error::Invalid("a conversation needs another person"));
    }
    if people.len() > MAX_OTHERS {
        return Err(Error::Invalid(
            "a group conversation can include at most 9 other people",
        ));
    }
    Ok(people)
}

fn group_name(command: &v1::UpdateChannel) -> Result<String, Error> {
    if command.kind.is_some()
        || command.parent_id.is_some()
        || command.position.is_some()
        || command.topic.is_some()
        || command.voice.is_some()
    {
        return Err(Error::Invalid(
            "only the name of a group conversation can change",
        ));
    }
    let name = command.name.as_deref().unwrap_or("");
    crate::text::name(name, MAX_NAME).map_err(|error| match error {
        crate::text::NameError::Length => Error::Invalid("channel name must be 1 to 64 characters"),
        crate::text::NameError::Hidden => {
            Error::Invalid("channel name contains a hidden character")
        }
    })
}

fn required_id(bytes: &[u8]) -> Result<Id, Error> {
    id_from_bytes(bytes).map_err(|_| Error::Invalid("id must be 16 bytes"))
}

fn conversation(
    channel: &db::Channel,
    members: &[Id],
    last_message_id: Option<Id>,
    created: bool,
) -> Result<CommandReply, Error> {
    let wire = snapshot::wire_channel(channel, members, last_message_id)?;
    let events = if created {
        vec![server_envelope::Msg::ChannelCreated(v1::ChannelCreated {
            channel: Some(wire.clone()),
        })]
    } else {
        Vec::new()
    };
    Ok(CommandReply {
        ack: ack::Result::DmChannel(wire),
        events,
    })
}

fn updated(
    channel: &db::Channel,
    members: &[Id],
    last_message_id: Option<Id>,
) -> Result<CommandReply, Error> {
    let wire = snapshot::wire_channel(channel, members, last_message_id)?;
    Ok(CommandReply {
        ack: ack::Result::Empty(v1::Empty {}),
        events: vec![server_envelope::Msg::ChannelUpdated(v1::ChannelUpdated {
            channel: Some(wire),
        })],
    })
}
