//! Permission-checked join, mute, and the Welcome voice snapshot.

use std::{collections::HashMap, sync::Arc, time::Duration};

use parking_lot::{Mutex, RwLock};
use ss_core::{ChannelKind, Id, Permissions, VoiceProfile};
use ss_proto::{v1, validate::id_from_bytes};

use crate::{
    db::{self, NewAuditEntry, audit_log, channels, users},
    handlers::Error,
    media::Forwarder,
    perms::{AccessError, PermissionService},
    voice::rooms::{self, JoinError, JoinPlan, Placement, Rooms},
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
    rooms: Arc<RwLock<Rooms>>,
    streams: Arc<crate::media::streams::Streams>,
    leases: Mutex<Leases>,
}

#[derive(Clone, Copy)]
struct Lease {
    conn: Id,
    user: Id,
    /// Which grant this is. A resume keeps the connection id, so a second
    /// disconnect leases the same `conn` again; only the timer started for
    /// this grant may end it.
    grant: u64,
}

/// Seats held for a resume, by session token.
#[derive(Default)]
struct Leases {
    held: HashMap<String, Lease>,
    grants: u64,
}

impl Leases {
    /// Holds `conn`'s seat for `token`. Returns the grant its timer ends.
    fn grant(&mut self, token: String, conn: Id, user: Id) -> u64 {
        self.grants += 1;
        let grant = self.grants;
        self.held.insert(token, Lease { conn, user, grant });
        grant
    }

    /// Takes the lease `token` holds for `user`, ending it.
    fn take(&mut self, token: &str, user: Id) -> Option<Lease> {
        match self.held.get(token) {
            Some(lease) if lease.user == user => self.held.remove(token),
            _ => None,
        }
    }

    /// Ends `grant` if `token` still holds it. `true` when its seat must now
    /// be dropped; a lease granted again since is left alone.
    fn expire(&mut self, token: &str, grant: u64) -> bool {
        let expired = self
            .held
            .get(token)
            .is_some_and(|lease| lease.grant == grant);
        if expired {
            self.held.remove(token);
        }
        expired
    }
}

const RESUME_LEASE: Duration = Duration::from_secs(60);

impl Voice {
    pub(crate) fn open(
        perms: Arc<PermissionService>,
        registry: Arc<crate::registry::ConnectionRegistry>,
    ) -> (Arc<Self>, Arc<Forwarder>) {
        let rooms = Arc::new(RwLock::new(Rooms::new(registry)));
        let streams = Arc::new(crate::media::streams::Streams::new(
            Arc::clone(&rooms),
            Arc::clone(&perms),
        ));
        let voice = Arc::new(Self {
            streams,
            perms,
            rooms: Arc::clone(&rooms),
            leases: Mutex::new(Leases::default()),
        });
        (voice, Arc::new(Forwarder::new(rooms)))
    }

    pub(crate) fn streams(&self) -> Arc<crate::media::streams::Streams> {
        Arc::clone(&self.streams)
    }

    /// Seats currently in a voice channel.
    #[must_use]
    pub fn member_count(&self) -> usize {
        self.rooms.read().member_count()
    }

    pub(crate) fn attach(&self, conn: Id, user: Id, connection: quinn::Connection) {
        self.rooms.write().attach(conn, user, connection);
    }

    /// Reattaches a leased seat when the member may still hold it.
    ///
    /// `Ok` is the resumed connection id. `Err` carries the leave events for
    /// a leased seat that was dropped because the member lost access (or the
    /// seat is already gone); the caller publishes them and attaches fresh.
    fn resume(
        &self,
        token: &str,
        user: Id,
        connection: quinn::Connection,
    ) -> Result<Id, Vec<v1::server_envelope::Msg>> {
        let lease = self.leases.lock().take(token, user);
        let Some(lease) = lease else {
            return Err(Vec::new());
        };
        let mut rooms = self.rooms.write();
        let allowed = rooms
            .seat(lease.conn)
            .is_some_and(|seat| self.may_hold(user, seat.channel, seat.flags.moved()));
        if allowed && rooms.reattach(lease.conn, user, connection) {
            return Ok(lease.conn);
        }
        Err(rooms.detach(lease.conn))
    }

    /// Attaches `conn`, or the leased seat `token` names, and returns the
    /// connection id in use. The events are the leave of a leased seat the
    /// member may no longer hold; the caller publishes them.
    pub(crate) fn connect(
        &self,
        conn: Id,
        token: &str,
        user: Id,
        connection: quinn::Connection,
    ) -> (Id, Vec<v1::server_envelope::Msg>) {
        match self.resume(token, user, connection.clone()) {
            Ok(resumed) => (resumed, Vec::new()),
            Err(events) => {
                self.attach(conn, user, connection);
                (conn, events)
            }
        }
    }

    pub(crate) fn lease(
        self: &Arc<Self>,
        token: String,
        conn: Id,
        user: Id,
        bus: crate::events::EventBus,
    ) -> bool {
        if !self.rooms.read().is_seated(conn) {
            return false;
        }
        let granted = self.leases.lock().grant(token.clone(), conn, user);
        let voice = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(RESUME_LEASE).await;
            if !voice.leases.lock().expire(&token, granted) {
                return;
            }
            for event in voice.detach(conn) {
                bus.publish(None, event);
            }
        });
        true
    }

    pub(crate) fn detach(&self, conn: Id) -> Vec<v1::server_envelope::Msg> {
        self.rooms.write().detach(conn)
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

    /// Rechecks every seat after a permission change. A member who lost
    /// `VIEW_CHANNEL`, or `CONNECT` on a seat they joined themselves, is
    /// removed; the rest get their speak bit updated. Video publishers and
    /// viewers are rechecked too. Returns the leave and removal events.
    pub(crate) fn refresh_seats(&self) -> Vec<v1::server_envelope::Msg> {
        let mut rooms = self.rooms.write();
        let mut events = rooms.video.refresh(&self.perms);
        events.extend(rooms.refresh(|user, channel, moved| {
            self.may_hold(user, channel, moved).then(|| {
                self.perms
                    .check(user, Some(channel), Permissions::SPEAK)
                    .is_ok()
            })
        }));
        events
    }

    fn sees(&self, user: Id, channel: Id) -> bool {
        self.perms
            .check(user, Some(channel), Permissions::VIEW_CHANNEL)
            .is_ok()
    }

    /// Whether `user` may keep a seat in `channel`. A moved seat skips `CONNECT`.
    fn may_hold(&self, user: Id, channel: Id, moved: bool) -> bool {
        let needed = if moved {
            Permissions::VIEW_CHANNEL
        } else {
            Permissions::VIEW_CHANNEL | Permissions::CONNECT
        };
        self.perms.check(user, Some(channel), needed).is_ok()
    }

    /// Drop every seat in a channel that is about to disappear.
    pub(crate) fn evacuate(&self, channel: Id) -> Vec<v1::server_envelope::Msg> {
        let conns = self.rooms.read().channel_conns(channel);
        let mut events = Vec::new();
        for conn in conns {
            events.extend(self.rooms.write().leave(conn));
        }
        events
    }

    pub(crate) fn fill(&self, snapshot: &mut v1::Snapshot, viewer: Id) {
        let perms = Arc::clone(&self.perms);
        let (states, tracks) = self.rooms.read().snapshot(|channel| {
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
            .write()
            .join(JoinPlan {
                conn,
                channel: channel_id,
                self_mute: command.self_mute,
                self_deaf: command.self_deaf,
                codec_channels: rooms::codec_channels(profile),
                max_bitrate_bps: profile.encoder_params().max_bitrate_bps,
                can_speak: self
                    .perms
                    .check(user, Some(channel_id), Permissions::SPEAK)
                    .is_ok(),
                placement: Placement::Joined,
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
            broadcast: self.rooms.write().leave(conn),
            direct: Vec::new(),
        }
    }

    fn set_flags(&self, conn: Id, self_mute: bool, self_deaf: bool) -> Result<Outcome, Error> {
        let broadcast = self
            .rooms
            .write()
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
            .write()
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
    /// The caller has already required `MOVE_MEMBERS`. `actor` must see both
    /// the destination and the channel `target` sits in: a destination they
    /// cannot see is not found, and a target in a channel they cannot see is
    /// not in voice as far as they can tell. `target` still needs view.
    /// Connect and the user limit are bypassed.
    pub(crate) async fn move_member(
        &self,
        actor: Id,
        target: Id,
        channel_id: Id,
    ) -> Result<(Outcome, Id), Error> {
        let channel = self.channel(channel_id).await?;
        if !self.sees(actor, channel_id) {
            return Err(Error::NotFound("unknown channel"));
        }
        if channel.kind != ChannelKind::Voice {
            return Err(Error::Invalid("not a voice channel"));
        }
        self.allow(
            target,
            channel_id,
            Permissions::VIEW_CHANNEL,
            "missing view channel",
        )?;
        let placed = self.rooms.read().placed(target);
        let (conn, self_mute, self_deaf) = placed
            .filter(|(conn, _, _)| {
                let current = self.rooms.read().seat(*conn);
                current.is_some_and(|seat| self.sees(actor, seat.channel))
            })
            .ok_or(Error::Invalid("not in a voice channel"))?;
        let profile = channel.voice_profile.unwrap_or(VoiceProfile::Voice);
        let events = self
            .rooms
            .write()
            .join(JoinPlan {
                conn,
                channel: channel_id,
                self_mute,
                self_deaf,
                codec_channels: rooms::codec_channels(profile),
                max_bitrate_bps: profile.encoder_params().max_bitrate_bps,
                can_speak: self
                    .perms
                    .check(target, Some(channel_id), Permissions::SPEAK)
                    .is_ok(),
                placement: Placement::Moved,
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

#[cfg(test)]
mod tests {
    use ss_core::Id;

    use super::Leases;

    #[test]
    fn a_first_lease_timer_does_not_end_a_second_lease() {
        let mut leases = Leases::default();
        let (conn, user) = (Id::generate(), Id::generate());
        // Disconnect: the seat is leased and a 60 s timer starts.
        let first = leases.grant("token".into(), conn, user);
        // Resume within the lease keeps the same connection id...
        assert!(leases.take("token", user).is_some());
        // ...and a second disconnect leases the seat again, with its own timer.
        let second = leases.grant("token".into(), conn, user);
        // The first timer fires: the second lease must survive it.
        assert!(
            !leases.expire("token", first),
            "the first lease's timer ended the second lease early"
        );
        assert!(leases.expire("token", second));
        assert!(leases.take("token", user).is_none());
    }
}
