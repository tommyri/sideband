//! Permission-checked join, mute, and the Welcome voice snapshot.

use std::sync::Arc;

use parking_lot::Mutex;
use ss_core::{ChannelKind, Id, Permissions, VoiceProfile};
use ss_proto::{v1, validate::id_from_bytes};

use crate::{
    db::{self, NewAuditEntry, audit_log, channels, users},
    handlers::Error,
    media::Forwarder,
    perms::{AccessError, PermissionService},
    voice::rooms::{self, JoinError, JoinPlan, Rooms},
};

/// Events for the actor's command reply. `broadcast` is also published.
pub(crate) struct Outcome {
    pub ack: v1::ack::Result,
    pub broadcast: Vec<v1::server_envelope::Msg>,
    pub direct: Vec<v1::server_envelope::Msg>,
}

/// Authenticated voice sessions. One user occupies one connection.
pub struct Voice {
    perms: Arc<PermissionService>,
    rooms: Arc<Mutex<Rooms>>,
    streams: Arc<crate::media::streams::Streams>,
}

impl Voice {
    pub(crate) fn open(
        perms: Arc<PermissionService>,
        registry: Arc<crate::registry::ConnectionRegistry>,
    ) -> (Arc<Self>, Arc<Forwarder>) {
        let rooms = Arc::new(Mutex::new(Rooms::new(registry)));
        let streams = Arc::new(crate::media::streams::Streams::new(
            Arc::clone(&rooms),
            Arc::clone(&perms),
        ));
        let voice = Arc::new(Self {
            streams,
            perms,
            rooms: Arc::clone(&rooms),
        });
        (voice, Arc::new(Forwarder::new(rooms)))
    }

    pub(crate) fn streams(&self) -> Arc<crate::media::streams::Streams> {
        Arc::clone(&self.streams)
    }

    /// Seats currently in a voice channel.
    #[must_use]
    pub fn member_count(&self) -> usize {
        self.rooms.lock().member_count()
    }

    pub(crate) fn attach(&self, conn: Id, user: Id, connection: quinn::Connection) {
        self.rooms.lock().attach(conn, user, connection);
    }

    pub(crate) fn detach(&self, conn: Id) -> Vec<v1::server_envelope::Msg> {
        self.rooms.lock().detach(conn)
    }

    /// `Some` when `msg` is a voice command this task owns.
    pub(crate) async fn dispatch(
        &self,
        conn: Id,
        user: Id,
        msg: &v1::client_envelope::Msg,
    ) -> Option<Result<Outcome, Error>> {
        if let Some(result) = self.streams.dispatch(conn, user, msg) {
            return Some(result);
        }
        match msg {
            v1::client_envelope::Msg::JoinVoice(command) => {
                Some(self.join(conn, user, command).await)
            }
            v1::client_envelope::Msg::LeaveVoice(_) => Some(Ok(self.leave(conn))),
            v1::client_envelope::Msg::SetVoiceState(command) => {
                Some(self.set_flags(conn, command.self_mute, command.self_deaf))
            }
            v1::client_envelope::Msg::ServerMuteUser(command) => {
                Some(self.server_mute(user, command).await)
            }
            _ => None,
        }
    }

    pub(crate) fn refresh_speak(&self) -> Vec<v1::server_envelope::Msg> {
        let perms = Arc::clone(&self.perms);
        self.rooms.lock().refresh_speak(move |user, channel| {
            perms.check(user, Some(channel), Permissions::SPEAK).is_ok()
        });
        self.rooms.lock().video.refresh(&self.perms)
    }

    /// Drop every seat in a channel that is about to disappear.
    pub(crate) fn evacuate(&self, channel: Id) -> Vec<v1::server_envelope::Msg> {
        let conns = self.rooms.lock().channel_conns(channel);
        let mut events = Vec::new();
        for conn in conns {
            events.extend(self.rooms.lock().leave(conn));
        }
        events
    }

    pub(crate) fn fill(&self, snapshot: &mut v1::Snapshot, viewer: Id) {
        let perms = Arc::clone(&self.perms);
        let (states, tracks) = self.rooms.lock().snapshot(|channel| {
            perms
                .check(viewer, Some(channel), Permissions::VIEW_CHANNEL)
                .is_ok()
        });
        snapshot.voice_states = states;
        snapshot.tracks = tracks;
    }

    async fn join(&self, conn: Id, user: Id, command: &v1::JoinVoice) -> Result<Outcome, Error> {
        let channel_id = id_from_bytes(&command.channel_id)
            .map_err(|_| Error::Invalid("channel id must be 16 bytes"))?;
        let channel = self.channel(channel_id).await?;
        if channel.kind != ChannelKind::Voice {
            return Err(Error::Invalid("not a voice channel"));
        }
        self.allow(
            user,
            channel_id,
            Permissions::VIEW_CHANNEL,
            "missing view channel",
        )?;
        self.allow(user, channel_id, Permissions::CONNECT, "missing connect")?;
        let bypass_limit = self
            .perms
            .check(user, Some(channel_id), Permissions::MOVE_MEMBERS)
            .is_ok();
        let profile = channel.voice_profile.unwrap_or(VoiceProfile::Voice);
        let events = self
            .rooms
            .lock()
            .join(JoinPlan {
                conn,
                channel: channel_id,
                self_mute: command.self_mute,
                self_deaf: command.self_deaf,
                codec_channels: rooms::codec_channels(profile),
                can_speak: self
                    .perms
                    .check(user, Some(channel_id), Permissions::SPEAK)
                    .is_ok(),
                limit: if bypass_limit {
                    None
                } else {
                    channel.user_limit
                },
            })
            .map_err(join_error)?;
        Ok(Outcome {
            ack: empty_ack(),
            broadcast: events.broadcast,
            direct: events.direct,
        })
    }

    fn leave(&self, conn: Id) -> Outcome {
        Outcome {
            ack: empty_ack(),
            broadcast: self.rooms.lock().leave(conn),
            direct: Vec::new(),
        }
    }

    fn set_flags(&self, conn: Id, self_mute: bool, self_deaf: bool) -> Result<Outcome, Error> {
        let broadcast = self
            .rooms
            .lock()
            .set_flags(conn, self_mute, self_deaf)
            .map_err(join_error)?;
        Ok(Outcome {
            ack: empty_ack(),
            broadcast,
            direct: Vec::new(),
        })
    }

    async fn server_mute(&self, actor: Id, command: &v1::ServerMuteUser) -> Result<Outcome, Error> {
        self.perms
            .check(actor, None, Permissions::MUTE_MEMBERS)
            .map_err(|_| Error::Forbidden("missing mute members"))?;
        let user = id_from_bytes(&command.user_id)
            .map_err(|_| Error::Invalid("user id must be 16 bytes"))?;
        // Same hierarchy rule as kick, ban, and move: a moderator cannot
        // silence someone whose highest role is at or above their own.
        if !self.perms.can_act_on(actor, user) {
            return Err(Error::Forbidden("cannot act on that member"));
        }
        let mut conn = self.perms.pool().read().await?;
        if users::get(&mut conn, user).await?.is_none() {
            return Err(Error::NotFound("unknown user"));
        }
        drop(conn);
        let broadcast = self
            .rooms
            .lock()
            .server_mute(user, command.muted)
            .into_iter()
            .collect();
        let mut conn = self.perms.pool().write().await;
        audit_log::insert(
            &mut conn,
            NewAuditEntry {
                id: Id::generate(),
                actor_id: Some(actor),
                action: "user.mute".to_owned(),
                target_kind: "user".to_owned(),
                target_id: Some(user),
                details: serde_json::json!({ "muted": command.muted }).to_string(),
            },
        )
        .await?;
        Ok(Outcome {
            ack: empty_ack(),
            broadcast,
            direct: Vec::new(),
        })
    }

    /// Moves `target`, who is already in voice, into `channel_id`.
    ///
    /// The caller has already required `MOVE_MEMBERS`. View is still required.
    /// Connect and the user limit are bypassed.
    pub(crate) async fn move_member(
        &self,
        target: Id,
        channel_id: Id,
    ) -> Result<(Outcome, Id), Error> {
        let channel = self.channel(channel_id).await?;
        if channel.kind != ChannelKind::Voice {
            return Err(Error::Invalid("not a voice channel"));
        }
        self.allow(
            target,
            channel_id,
            Permissions::VIEW_CHANNEL,
            "missing view channel",
        )?;
        let (conn, self_mute, self_deaf) = self
            .rooms
            .lock()
            .placed(target)
            .ok_or(Error::Invalid("not in a voice channel"))?;
        let profile = channel.voice_profile.unwrap_or(VoiceProfile::Voice);
        let events = self
            .rooms
            .lock()
            .join(JoinPlan {
                conn,
                channel: channel_id,
                self_mute,
                self_deaf,
                codec_channels: rooms::codec_channels(profile),
                can_speak: self
                    .perms
                    .check(target, Some(channel_id), Permissions::SPEAK)
                    .is_ok(),
                limit: None,
            })
            .map_err(join_error)?;
        Ok((
            Outcome {
                ack: empty_ack(),
                broadcast: events.broadcast,
                direct: events.direct,
            },
            conn,
        ))
    }

    async fn channel(&self, id: Id) -> Result<db::Channel, Error> {
        let mut conn = self.perms.pool().read().await?;
        let channel = channels::get(&mut conn, id)
            .await?
            .ok_or(Error::NotFound("unknown channel"))?;
        if channel.deleted_at.is_some() {
            return Err(Error::NotFound("unknown channel"));
        }
        Ok(channel)
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
                AccessError::NotFound => Error::NotFound("unknown channel"),
                AccessError::Forbidden => Error::Forbidden(message),
            })
    }
}

fn join_error(error: JoinError) -> Error {
    match error {
        JoinError::NotAttached => Error::Invalid("unknown connection"),
        JoinError::NotInVoice => Error::Invalid("not in a voice channel"),
        JoinError::Full => Error::Forbidden("voice channel is full"),
        JoinError::TracksExhausted => Error::Invalid("track id space exhausted"),
    }
}

fn empty_ack() -> v1::ack::Result {
    v1::ack::Result::Empty(v1::Empty {})
}
