//! `CreateChannel`, `UpdateChannel`, `DeleteChannel`, and `MoveChannel`.
//!
//! All four require `MANAGE_CHANNELS`. Sibling positions are dense and start
//! at 0 for each parent. Deleting a category soft-deletes its descendants.
//! Messages stay in the database. Voice sessions are T-025, so a delete does
//! not emit `VoiceStateUpdated`.

use std::{collections::HashMap, sync::Arc};

use sqlx::Connection;
use ss_core::{ChannelKind, Id, Permissions, Timestamp, VoiceProfile};
use ss_proto::{
    v1::{self, ack, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};

use crate::{
    db::{self, ChannelUpdate, NewAuditEntry, NewChannel, audit_log, channels},
    perms::PermissionService,
    snapshot,
};

use super::{CommandReply, Error};

/// Channel commands. Mutations reload [`PermissionService`].
pub struct Channels {
    perms: Arc<PermissionService>,
}

impl Channels {
    /// Shares `perms` with role commands and the snapshot.
    #[must_use]
    pub fn new(perms: Arc<PermissionService>) -> Self {
        Self { perms }
    }

    /// Dispatches a channel command. Other messages return `None`.
    pub async fn dispatch(
        &self,
        actor: Id,
        msg: &v1::client_envelope::Msg,
    ) -> Option<Result<CommandReply, Error>> {
        match msg {
            v1::client_envelope::Msg::CreateChannel(command) => {
                Some(self.create(actor, command).await)
            }
            v1::client_envelope::Msg::UpdateChannel(command) => {
                Some(self.update(actor, command).await)
            }
            v1::client_envelope::Msg::DeleteChannel(command) => {
                Some(self.delete(actor, command).await)
            }
            v1::client_envelope::Msg::MoveChannel(command) => {
                Some(self.relocate(actor, command).await)
            }
            _ => None,
        }
    }

    /// Creates a category, text channel, or voice channel.
    pub async fn create(
        &self,
        actor: Id,
        command: &v1::CreateChannel,
    ) -> Result<CommandReply, Error> {
        self.require(actor)?;
        let kind = guild_kind(command.kind)?;
        let name = channel_name(&command.name)?.to_owned();
        let parent = optional_id(command.parent_id.as_deref())?;
        if kind == ChannelKind::Category && parent.is_some() {
            return Err(Error::Invalid("categories cannot be nested"));
        }
        let topic = stored_topic(command.topic.as_deref());
        let voice = voice_columns(kind, command.voice.as_ref())?;
        let id = Id::generate();
        let channel = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            require_category(&mut tx, parent).await?;
            channels::insert(
                &mut tx,
                NewChannel {
                    id,
                    kind,
                    name,
                    topic,
                    parent_id: parent,
                    position: 0,
                    voice_profile: voice.profile,
                    voice_bitrate: voice.bitrate,
                    user_limit: voice.limit,
                    created_by: actor,
                },
            )
            .await?;
            insert_at(&mut tx, parent, id, command.position).await?;
            let channel = reload_channel(&mut tx, id).await?;
            audit(
                &mut tx,
                actor,
                "channel.create",
                channel.id,
                &format!(r#"{{"position":{}}}"#, channel.position),
            )
            .await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.reload().await?;
            channel
        };
        tracing::info!(%actor, channel_id = %channel.id, "channel created");
        reply_channel(
            ack::Result::ChannelId(id_to_bytes(channel.id)),
            &channel,
            true,
        )
    }

    /// Patches a guild channel. Moving to the root is [`Self::relocate`].
    pub async fn update(
        &self,
        actor: Id,
        command: &v1::UpdateChannel,
    ) -> Result<CommandReply, Error> {
        self.require(actor)?;
        let id = required_id(&command.channel_id)?;
        if let Some(name) = &command.name {
            channel_name(name)?;
        }
        let channel = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let current = live_channel(&mut tx, id).await?;
            let kind = match command.kind {
                Some(value) => guild_kind(value)?,
                None => current.kind,
            };
            let name = match &command.name {
                Some(name) => channel_name(name)?.to_owned(),
                None => current.name.clone(),
            };
            let topic = match command.topic.as_deref() {
                None => current.topic.clone(),
                Some(topic) => stored_topic(Some(topic)),
            };
            let parent = match command.parent_id.as_deref() {
                None => current.parent_id,
                Some(bytes) => Some(required_id(bytes)?),
            };
            let voice = if command.voice.is_some() || kind != current.kind {
                voice_columns(kind, command.voice.as_ref())?
            } else {
                VoiceColumns {
                    profile: current.voice_profile,
                    bitrate: current.voice_bitrate,
                    limit: current.user_limit,
                }
            };
            prepare_parent(&mut tx, id, kind, parent).await?;
            if parent != current.parent_id {
                detach(&mut tx, current.parent_id, id).await?;
            }
            channels::update(
                &mut tx,
                id,
                ChannelUpdate {
                    kind,
                    name,
                    topic,
                    parent_id: parent,
                    position: current.position,
                    voice_profile: voice.profile,
                    voice_bitrate: voice.bitrate,
                    user_limit: voice.limit,
                },
            )
            .await?;
            let position = command
                .position
                .unwrap_or(i32::try_from(current.position).unwrap_or(0));
            if parent != current.parent_id || command.position.is_some() {
                insert_at(&mut tx, parent, id, position).await?;
            }
            let channel = reload_channel(&mut tx, id).await?;
            audit(&mut tx, actor, "channel.update", id, "{}").await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.reload().await?;
            channel
        };
        tracing::info!(%actor, channel_id = %channel.id, "channel updated");
        reply_channel(ack::Result::Empty(v1::Empty::default()), &channel, false)
    }

    /// Soft-deletes a channel and its descendants. Messages remain stored.
    pub async fn delete(
        &self,
        actor: Id,
        command: &v1::DeleteChannel,
    ) -> Result<CommandReply, Error> {
        self.require(actor)?;
        let id = required_id(&command.channel_id)?;
        let events = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let current = live_channel(&mut tx, id).await?;
            let all = channels::list(&mut tx).await?;
            let doomed = subtree(&all, id);
            let mut wires = Vec::with_capacity(doomed.len());
            for doomed_id in &doomed {
                let channel = all
                    .iter()
                    .find(|channel| channel.id == *doomed_id)
                    .ok_or(Error::NotFound("channel not found"))?;
                wires.push(snapshot::wire_channel(channel, &[])?);
            }
            let now = Timestamp::now();
            for doomed_id in &doomed {
                channels::soft_delete(&mut tx, *doomed_id, now).await?;
            }
            compact(&mut tx, current.parent_id).await?;
            audit(&mut tx, actor, "channel.delete", id, "{}").await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.reload().await?;
            wires
        };
        tracing::info!(%actor, channel_id = %id, "channel deleted");
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            events: events
                .into_iter()
                .map(|channel| {
                    server_envelope::Msg::ChannelDeleted(v1::ChannelDeleted {
                        channel: Some(channel),
                    })
                })
                .collect(),
        })
    }

    /// Moves a channel to a parent, or to the root when the parent is absent.
    pub async fn relocate(
        &self,
        actor: Id,
        command: &v1::MoveChannel,
    ) -> Result<CommandReply, Error> {
        self.require(actor)?;
        let id = required_id(&command.channel_id)?;
        let parent = optional_id(command.parent_id.as_deref())?;
        let channel = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let current = live_channel(&mut tx, id).await?;
            prepare_parent(&mut tx, id, current.kind, parent).await?;
            if parent != current.parent_id {
                detach(&mut tx, current.parent_id, id).await?;
            }
            insert_at(&mut tx, parent, id, command.position).await?;
            let channel = reload_channel(&mut tx, id).await?;
            audit(
                &mut tx,
                actor,
                "channel.move",
                id,
                &format!(r#"{{"position":{}}}"#, channel.position),
            )
            .await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.reload().await?;
            channel
        };
        tracing::info!(%actor, channel_id = %channel.id, "channel moved");
        reply_channel(ack::Result::Empty(v1::Empty::default()), &channel, false)
    }

    fn require(&self, actor: Id) -> Result<(), Error> {
        self.perms
            .check(actor, None, Permissions::MANAGE_CHANNELS)
            .map_err(|_| Error::Forbidden("missing permission"))
    }
}

async fn prepare_parent(
    conn: &mut sqlx::SqliteConnection,
    channel: Id,
    kind: ChannelKind,
    parent: Option<Id>,
) -> Result<(), Error> {
    if kind == ChannelKind::Category && parent.is_some() {
        return Err(Error::Invalid("categories cannot be nested"));
    }
    require_category(conn, parent).await?;
    let all = channels::list(conn).await?;
    if cycles(&all, channel, parent) {
        return Err(Error::Invalid("a channel cannot be moved under itself"));
    }
    if kind != ChannelKind::Category && has_children(&all, channel) {
        return Err(Error::Invalid("only a category can contain channels"));
    }
    Ok(())
}

fn reply_channel(
    ack_body: ack::Result,
    channel: &db::Channel,
    created: bool,
) -> Result<CommandReply, Error> {
    let wire = snapshot::wire_channel(channel, &[])?;
    let event = if created {
        server_envelope::Msg::ChannelCreated(v1::ChannelCreated {
            channel: Some(wire),
        })
    } else {
        server_envelope::Msg::ChannelUpdated(v1::ChannelUpdated {
            channel: Some(wire),
        })
    };
    Ok(CommandReply {
        ack: ack_body,
        events: vec![event],
    })
}

async fn live_channel(conn: &mut sqlx::SqliteConnection, id: Id) -> Result<db::Channel, Error> {
    let channel = channels::get(conn, id)
        .await?
        .ok_or(Error::NotFound("channel not found"))?;
    if channel.deleted_at.is_some() || channel.kind.is_direct() {
        return Err(Error::NotFound("channel not found"));
    }
    Ok(channel)
}

async fn reload_channel(conn: &mut sqlx::SqliteConnection, id: Id) -> Result<db::Channel, Error> {
    channels::get(conn, id)
        .await?
        .ok_or(Error::NotFound("channel not found"))
}

async fn require_category(
    conn: &mut sqlx::SqliteConnection,
    parent: Option<Id>,
) -> Result<(), Error> {
    let Some(parent) = parent else {
        return Ok(());
    };
    let channel = live_channel(conn, parent).await?;
    if channel.kind == ChannelKind::Category {
        Ok(())
    } else {
        Err(Error::Invalid("parent must be a category"))
    }
}

async fn sibling_ids(
    conn: &mut sqlx::SqliteConnection,
    parent: Option<Id>,
) -> Result<Vec<Id>, Error> {
    let mut kids: Vec<_> = channels::list(conn)
        .await?
        .into_iter()
        .filter(|channel| channel.parent_id == parent && !channel.kind.is_direct())
        .collect();
    kids.sort_by_key(|channel| (channel.position, channel.id));
    Ok(kids.into_iter().map(|channel| channel.id).collect())
}

async fn write_order(
    conn: &mut sqlx::SqliteConnection,
    parent: Option<Id>,
    ids: &[Id],
) -> Result<(), Error> {
    for (index, id) in ids.iter().enumerate() {
        let position = i64::try_from(index).map_err(|_| db::Error::Corrupt("channel position"))?;
        channels::place(conn, *id, parent, position).await?;
    }
    Ok(())
}

async fn insert_at(
    conn: &mut sqlx::SqliteConnection,
    parent: Option<Id>,
    id: Id,
    position: i32,
) -> Result<(), Error> {
    let mut ids = sibling_ids(conn, parent).await?;
    ids.retain(|existing| *existing != id);
    let index = clamp_index(position, ids.len());
    ids.insert(index, id);
    write_order(conn, parent, &ids).await
}

async fn detach(
    conn: &mut sqlx::SqliteConnection,
    parent: Option<Id>,
    id: Id,
) -> Result<(), Error> {
    let mut ids = sibling_ids(conn, parent).await?;
    ids.retain(|existing| *existing != id);
    write_order(conn, parent, &ids).await
}

async fn compact(conn: &mut sqlx::SqliteConnection, parent: Option<Id>) -> Result<(), Error> {
    let ids = sibling_ids(conn, parent).await?;
    write_order(conn, parent, &ids).await
}

fn subtree(all: &[db::Channel], root: Id) -> Vec<Id> {
    let mut doomed = vec![root];
    let mut grew = true;
    while grew {
        grew = false;
        for channel in all {
            if channel
                .parent_id
                .is_some_and(|parent| doomed.contains(&parent))
                && !doomed.contains(&channel.id)
            {
                doomed.push(channel.id);
                grew = true;
            }
        }
    }
    doomed
}

fn has_children(all: &[db::Channel], id: Id) -> bool {
    all.iter().any(|channel| channel.parent_id == Some(id))
}

fn cycles(all: &[db::Channel], channel: Id, parent: Option<Id>) -> bool {
    let mut current = parent;
    let parents: HashMap<Id, Option<Id>> = all.iter().map(|row| (row.id, row.parent_id)).collect();
    let mut steps = 0_usize;
    while let Some(id) = current {
        if id == channel {
            return true;
        }
        current = parents.get(&id).copied().flatten();
        steps = steps.saturating_add(1);
        if steps > parents.len() {
            return true;
        }
    }
    false
}

fn clamp_index(position: i32, len: usize) -> usize {
    let position = i64::from(position).max(0);
    usize::try_from(position).unwrap_or(len).min(len)
}

fn channel_name(name: &str) -> Result<&str, Error> {
    let len = name.chars().count();
    if (1..=64).contains(&len) {
        Ok(name)
    } else {
        Err(Error::Invalid("channel name must be 1 to 64 characters"))
    }
}

fn stored_topic(topic: Option<&str>) -> Option<String> {
    topic.and_then(|topic| {
        if topic.is_empty() {
            None
        } else {
            Some(topic.to_owned())
        }
    })
}

fn guild_kind(value: i32) -> Result<ChannelKind, Error> {
    match v1::ChannelKind::try_from(value) {
        Ok(v1::ChannelKind::Category) => Ok(ChannelKind::Category),
        Ok(v1::ChannelKind::Text) => Ok(ChannelKind::Text),
        Ok(v1::ChannelKind::Voice) => Ok(ChannelKind::Voice),
        _ => Err(Error::Invalid(
            "channel kind must be category, text, or voice",
        )),
    }
}

fn optional_id(bytes: Option<&[u8]>) -> Result<Option<Id>, Error> {
    bytes.map(required_id).transpose()
}

fn required_id(bytes: impl AsRef<[u8]>) -> Result<Id, Error> {
    id_from_bytes(bytes.as_ref()).map_err(|_| Error::Invalid("id must be 16 bytes"))
}

struct VoiceColumns {
    profile: Option<VoiceProfile>,
    bitrate: Option<i64>,
    limit: Option<i64>,
}

fn voice_columns(
    kind: ChannelKind,
    voice: Option<&v1::VoiceSettings>,
) -> Result<VoiceColumns, Error> {
    if kind != ChannelKind::Voice {
        return if voice.is_some() {
            Err(Error::Invalid("voice settings belong on a voice channel"))
        } else {
            Ok(VoiceColumns {
                profile: None,
                bitrate: None,
                limit: None,
            })
        };
    }
    let profile = match voice.map(|settings| settings.profile) {
        None | Some(0) => VoiceProfile::Voice,
        Some(value) => profile_from_i32(value)?,
    };
    let bitrate = voice.and_then(|settings| settings.bitrate);
    let limit = voice.and_then(|settings| settings.user_limit);
    if let Some(bitrate) = bitrate {
        let params = profile.encoder_params();
        if bitrate < params.min_bitrate_bps || bitrate > params.max_bitrate_bps {
            return Err(Error::Invalid("bitrate is outside the profile range"));
        }
    }
    if limit.is_some_and(|limit| limit > 99) {
        return Err(Error::Invalid("user limit must be 0 to 99"));
    }
    Ok(VoiceColumns {
        profile: Some(profile),
        bitrate: bitrate.map(i64::from),
        limit: limit.map(i64::from),
    })
}

fn profile_from_i32(value: i32) -> Result<VoiceProfile, Error> {
    match v1::VoiceProfile::try_from(value) {
        Ok(v1::VoiceProfile::Voice | v1::VoiceProfile::Unspecified) => Ok(VoiceProfile::Voice),
        Ok(v1::VoiceProfile::Music) => Ok(VoiceProfile::Music),
        Ok(v1::VoiceProfile::LowLatency) => Ok(VoiceProfile::LowLatency),
        _ => Err(Error::Invalid("unknown voice profile")),
    }
}

async fn audit(
    conn: &mut sqlx::SqliteConnection,
    actor: Id,
    action: &'static str,
    channel_id: Id,
    details: &str,
) -> Result<(), Error> {
    audit_log::insert(
        conn,
        NewAuditEntry {
            id: Id::generate(),
            actor_id: Some(actor),
            action: action.to_owned(),
            target_kind: "channel".to_owned(),
            target_id: Some(channel_id),
            details: details.to_owned(),
        },
    )
    .await?;
    Ok(())
}
