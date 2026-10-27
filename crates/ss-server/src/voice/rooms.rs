//! In-memory voice seats. Forwarding and membership edits share one lock.

use std::collections::{HashMap, HashSet};

use ss_core::{Id, VoiceProfile};
use ss_proto::{v1, validate::id_to_bytes};

pub(crate) struct Rooms {
    peers: HashMap<Id, Peer>,
    by_user: HashMap<Id, Id>,
    channels: HashMap<Id, HashSet<Id>>,
    server_mute: HashSet<Id>,
    next_track: u32,
}

struct Peer {
    user: Id,
    connection: quinn::Connection,
    seat: Option<Seat>,
}

#[derive(Clone, Copy)]
pub(crate) struct Seat {
    pub channel: Id,
    pub track: u32,
    pub flags: Flags,
    pub codec_channels: u8,
}

/// Mute, deaf, and speak bits stored on a seat. Four separate `bool` fields
/// trip the pedantic struct lint without making the hot path clearer.
#[derive(Clone, Copy)]
pub(crate) struct Flags(u8);

impl Flags {
    const SELF_MUTE: u8 = 1;
    const SELF_DEAF: u8 = 1 << 1;
    const SERVER_MUTE: u8 = 1 << 2;
    const CAN_SPEAK: u8 = 1 << 3;

    pub(crate) fn arriving(self_mute: bool, self_deaf: bool, can_speak: bool) -> Self {
        Self(u8::from(self_mute) | (u8::from(self_deaf) << 1) | (u8::from(can_speak) << 3))
    }

    pub(crate) const fn self_mute(self) -> bool {
        self.0 & Self::SELF_MUTE != 0
    }

    pub(crate) const fn self_deaf(self) -> bool {
        self.0 & Self::SELF_DEAF != 0
    }

    pub(crate) const fn server_mute(self) -> bool {
        self.0 & Self::SERVER_MUTE != 0
    }

    pub(crate) const fn can_speak(self) -> bool {
        self.0 & Self::CAN_SPEAK != 0
    }

    fn with(self, bit: u8, enabled: bool) -> Self {
        if enabled {
            Self(self.0 | bit)
        } else {
            Self(self.0 & !bit)
        }
    }

    fn set_local(self, self_mute: bool, self_deaf: bool) -> Self {
        self.with(Self::SELF_MUTE, self_mute)
            .with(Self::SELF_DEAF, self_deaf)
    }

    fn with_server_mute(self, muted: bool) -> Self {
        self.with(Self::SERVER_MUTE, muted)
    }

    fn with_speak(self, allowed: bool) -> Self {
        self.with(Self::CAN_SPEAK, allowed)
    }
}

impl Rooms {
    pub(crate) fn new() -> Self {
        Self {
            peers: HashMap::new(),
            by_user: HashMap::new(),
            channels: HashMap::new(),
            server_mute: HashSet::new(),
            next_track: 0,
        }
    }

    pub(crate) fn attach(&mut self, conn: Id, user: Id, connection: quinn::Connection) {
        self.peers.insert(
            conn,
            Peer {
                user,
                connection,
                seat: None,
            },
        );
    }

    pub(crate) fn detach(&mut self, conn: Id) -> Vec<v1::server_envelope::Msg> {
        let events = self.leave(conn);
        self.peers.remove(&conn);
        events
    }

    pub(crate) fn member_count(&self) -> usize {
        self.peers
            .values()
            .filter(|peer| peer.seat.is_some())
            .count()
    }

    /// Connection and local mute flags for a user who is in voice.
    pub(crate) fn placed(&self, user: Id) -> Option<(Id, bool, bool)> {
        let conn = *self.by_user.get(&user)?;
        let seat = self.seat(conn)?;
        Some((conn, seat.flags.self_mute(), seat.flags.self_deaf()))
    }

    /// Hot-path lookup. Copies the seat so the caller can borrow the channel set.
    pub(crate) fn seat(&self, conn: Id) -> Option<Seat> {
        self.peers.get(&conn).and_then(|peer| peer.seat)
    }

    pub(crate) fn members(&self, channel: Id) -> Option<&HashSet<Id>> {
        self.channels.get(&channel)
    }

    /// Connection and deaf flag for one recipient.
    pub(crate) fn send_target(&self, conn: Id) -> Option<(&quinn::Connection, bool)> {
        let peer = self.peers.get(&conn)?;
        Some((
            &peer.connection,
            peer.seat.is_some_and(|seat| seat.flags.self_deaf()),
        ))
    }

    pub(crate) fn leave(&mut self, conn: Id) -> Vec<v1::server_envelope::Msg> {
        let (user, seat) = {
            let Some(peer) = self.peers.get_mut(&conn) else {
                return Vec::new();
            };
            let Some(seat) = peer.seat.take() else {
                return Vec::new();
            };
            (peer.user, seat)
        };
        if self.by_user.get(&user) == Some(&conn) {
            self.by_user.remove(&user);
        }
        if let Some(members) = self.channels.get_mut(&seat.channel) {
            members.remove(&conn);
            if members.is_empty() {
                self.channels.remove(&seat.channel);
            }
        }
        vec![
            track_removed(seat.track),
            voice_state(user, conn, None, false, false, seat.flags.server_mute()),
        ]
    }

    /// Move `plan.conn` into `plan.channel`, or update mute flags in place.
    /// A second connection for the same user is removed from voice first.
    pub(crate) fn join(&mut self, plan: JoinPlan) -> Result<JoinEvents, JoinError> {
        let JoinPlan {
            conn,
            channel,
            self_mute,
            self_deaf,
            codec_channels,
            can_speak,
            limit,
        } = plan;
        let Some(user) = self.peers.get(&conn).map(|peer| peer.user) else {
            return Err(JoinError::NotAttached);
        };
        let mut broadcast = Vec::new();
        if let Some(other) = self
            .by_user
            .get(&user)
            .copied()
            .filter(|other| *other != conn)
        {
            broadcast.extend(self.leave(other));
        }
        if self.seat(conn).is_some_and(|seat| seat.channel != channel) {
            broadcast.extend(self.leave(conn));
        }
        if self.seat(conn).is_some_and(|seat| seat.channel == channel) {
            return Ok(self.retune(conn, self_mute, self_deaf));
        }
        if let Some(limit) = limit.filter(|limit| *limit > 0) {
            let occupied = self.members(channel).map_or(0, HashSet::len);
            if i64::try_from(occupied).unwrap_or(i64::MAX) >= limit {
                return Err(JoinError::Full);
            }
        }
        let track = self
            .next_track
            .checked_add(1)
            .filter(|track| *track <= ss_net::media::MAX_TRACK_ID)
            .ok_or(JoinError::TracksExhausted)?;
        self.next_track = track;
        let server_mute = self.server_mute.contains(&user);
        let seat = Seat {
            channel,
            track,
            flags: Flags::arriving(self_mute, self_deaf, can_speak).with_server_mute(server_mute),
            codec_channels,
        };
        let direct = self.announce_existing(conn, channel);
        {
            let Some(peer) = self.peers.get_mut(&conn) else {
                return Err(JoinError::NotAttached);
            };
            peer.seat = Some(seat);
        }
        self.by_user.insert(user, conn);
        self.channels.entry(channel).or_default().insert(conn);
        broadcast.push(track_added(user, conn, seat));
        broadcast.push(voice_state(
            user,
            conn,
            Some(channel),
            self_mute,
            self_deaf,
            server_mute,
        ));
        Ok(JoinEvents { broadcast, direct })
    }

    pub(crate) fn set_flags(
        &mut self,
        conn: Id,
        self_mute: bool,
        self_deaf: bool,
    ) -> Result<Vec<v1::server_envelope::Msg>, JoinError> {
        let Some(peer) = self.peers.get_mut(&conn) else {
            return Err(JoinError::NotAttached);
        };
        let user = peer.user;
        let Some(seat) = peer.seat.as_mut() else {
            return Err(JoinError::NotInVoice);
        };
        seat.flags = seat.flags.set_local(self_mute, self_deaf);
        let channel = seat.channel;
        let server_mute = seat.flags.server_mute();
        Ok(vec![voice_state(
            user,
            conn,
            Some(channel),
            self_mute,
            self_deaf,
            server_mute,
        )])
    }

    pub(crate) fn server_mute(
        &mut self,
        user: Id,
        muted: bool,
    ) -> Option<v1::server_envelope::Msg> {
        if muted {
            self.server_mute.insert(user);
        } else {
            self.server_mute.remove(&user);
        }
        let conn = *self.by_user.get(&user)?;
        let peer = self.peers.get_mut(&conn)?;
        let seat = peer.seat.as_mut()?;
        seat.flags = seat.flags.with_server_mute(muted);
        let channel = seat.channel;
        let self_mute = seat.flags.self_mute();
        let self_deaf = seat.flags.self_deaf();
        Some(voice_state(
            user,
            conn,
            Some(channel),
            self_mute,
            self_deaf,
            muted,
        ))
    }

    pub(crate) fn refresh_speak(&mut self, speak: impl Fn(Id, Id) -> bool) {
        let plan: Vec<(Id, Id, Id)> = self
            .peers
            .iter()
            .filter_map(|(conn, peer)| peer.seat.map(|seat| (*conn, peer.user, seat.channel)))
            .collect();
        for (conn, user, channel) in plan {
            let allowed = speak(user, channel);
            if let Some(seat) = self
                .peers
                .get_mut(&conn)
                .and_then(|peer| peer.seat.as_mut())
            {
                seat.flags = seat.flags.with_speak(allowed);
            }
        }
    }

    pub(crate) fn channel_conns(&self, channel: Id) -> Vec<Id> {
        self.members(channel)
            .map(|members| members.iter().copied().collect())
            .unwrap_or_default()
    }

    pub(crate) fn snapshot(
        &self,
        visible: impl Fn(Id) -> bool,
    ) -> (Vec<v1::VoiceState>, Vec<v1::Track>) {
        let mut states = Vec::new();
        let mut tracks = Vec::new();
        for (conn, peer) in &self.peers {
            let Some(seat) = peer.seat else { continue };
            if !visible(seat.channel) {
                continue;
            }
            states.push(state_message(
                peer.user,
                *conn,
                Some(seat.channel),
                seat.flags.self_mute(),
                seat.flags.self_deaf(),
                seat.flags.server_mute(),
            ));
            tracks.push(track_message(peer.user, *conn, seat));
        }
        (states, tracks)
    }

    fn retune(&mut self, conn: Id, self_mute: bool, self_deaf: bool) -> JoinEvents {
        let events = self
            .set_flags(conn, self_mute, self_deaf)
            .unwrap_or_default();
        JoinEvents {
            broadcast: events,
            direct: Vec::new(),
        }
    }

    fn announce_existing(&self, joiner: Id, channel: Id) -> Vec<v1::server_envelope::Msg> {
        let Some(members) = self.members(channel) else {
            return Vec::new();
        };
        let mut events = Vec::new();
        for conn in members {
            if *conn == joiner {
                continue;
            }
            let Some(peer) = self.peers.get(conn) else {
                continue;
            };
            let Some(seat) = peer.seat else { continue };
            events.push(track_added(peer.user, *conn, seat));
            events.push(voice_state(
                peer.user,
                *conn,
                Some(seat.channel),
                seat.flags.self_mute(),
                seat.flags.self_deaf(),
                seat.flags.server_mute(),
            ));
        }
        events
    }
}

#[derive(Clone, Copy)]
pub(crate) struct JoinPlan {
    pub conn: Id,
    pub channel: Id,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub can_speak: bool,
    pub codec_channels: u8,
    /// `None` or `Some(0)` means no cap. Callers that may move members pass `None`.
    pub limit: Option<i64>,
}

pub(crate) struct JoinEvents {
    pub broadcast: Vec<v1::server_envelope::Msg>,
    pub direct: Vec<v1::server_envelope::Msg>,
}

#[derive(Clone, Copy)]
pub(crate) enum JoinError {
    NotAttached,
    NotInVoice,
    Full,
    TracksExhausted,
}

fn voice_state(
    user: Id,
    conn: Id,
    channel: Option<Id>,
    self_mute: bool,
    self_deaf: bool,
    server_mute: bool,
) -> v1::server_envelope::Msg {
    v1::server_envelope::Msg::VoiceStateUpdated(v1::VoiceStateUpdated {
        user_id: id_to_bytes(user),
        conn_id: id_to_bytes(conn),
        channel_id: channel.map(id_to_bytes),
        self_mute,
        self_deaf,
        server_mute,
    })
}

fn state_message(
    user: Id,
    conn: Id,
    channel: Option<Id>,
    self_mute: bool,
    self_deaf: bool,
    server_mute: bool,
) -> v1::VoiceState {
    v1::VoiceState {
        user_id: id_to_bytes(user),
        conn_id: id_to_bytes(conn),
        channel_id: channel.map(id_to_bytes),
        self_mute,
        self_deaf,
        server_mute,
    }
}

fn track_added(user: Id, conn: Id, seat: Seat) -> v1::server_envelope::Msg {
    v1::server_envelope::Msg::TrackAdded(v1::TrackAdded {
        track_id: seat.track,
        user_id: id_to_bytes(user),
        conn_id: id_to_bytes(conn),
        kind: i32::from(v1::TrackKind::Audio),
        codec: Some(codec(seat.codec_channels)),
        channel_id: id_to_bytes(seat.channel),
    })
}

fn track_message(user: Id, conn: Id, seat: Seat) -> v1::Track {
    v1::Track {
        track_id: seat.track,
        user_id: id_to_bytes(user),
        conn_id: id_to_bytes(conn),
        kind: i32::from(v1::TrackKind::Audio),
        codec: Some(codec(seat.codec_channels)),
        channel_id: id_to_bytes(seat.channel),
    }
}

fn track_removed(track: u32) -> v1::server_envelope::Msg {
    v1::server_envelope::Msg::TrackRemoved(v1::TrackRemoved { track_id: track })
}

fn codec(channels: u8) -> v1::Codec {
    v1::Codec {
        name: "opus".into(),
        sample_rate: 48_000,
        channels: u32::from(channels),
        description: bytes::Bytes::new(),
    }
}

pub(crate) fn codec_channels(profile: VoiceProfile) -> u8 {
    profile.encoder_params().channels
}
