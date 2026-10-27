//! Connection-owned voice membership. Control changes and fan-out share one lock
//! so a leave cannot race with forwarding into the old channel.

use std::collections::{HashMap, HashSet};

use parking_lot::Mutex;
use ss_core::Id;
use ss_proto::{
    v1::{self, server_envelope::Msg},
    validate::id_to_bytes,
};
use tokio::sync::mpsc;

/// Two events per member in the largest bootstrap, two departure events when
/// switching, plus Welcome/Ack headroom. A valid join must fit before the writer
/// gets another poll; slow consumers still hit a finite queue limit.
pub(crate) const CONTROL_CAPACITY: usize = 2 * crate::MAX_CONNECTIONS + 4;

/// Invalid membership transition.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The connection has not completed Hello.
    #[error("unknown connection")]
    UnknownConnection,
    /// Track ids are never reused during one server lifetime.
    #[error("track id space exhausted; restart the spike server")]
    TracksExhausted,
}

/// Shared membership and track ownership; no persistence or authorization yet.
#[derive(Default)]
pub struct Registry {
    pub(crate) state: Mutex<State>,
}

#[derive(Default)]
pub(crate) struct State {
    pub peers: HashMap<Id, Peer>,
    pub channels: HashMap<Id, HashSet<Id>>,
    next_track: u32,
}

pub(crate) struct Peer {
    pub connection: quinn::Connection,
    pub events: mpsc::Sender<v1::ServerEnvelope>,
    user: Id,
    pub membership: Option<Membership>,
}

#[derive(Clone, Copy)]
pub(crate) struct Membership {
    pub channel: Id,
    pub track: u32,
    pub muted: bool,
    pub deaf: bool,
}

impl Peer {
    fn emit(&self, message: Msg) {
        self.send(v1::ServerEnvelope {
            request_id: 0,
            msg: Some(message),
        });
    }

    pub(crate) fn send(&self, envelope: v1::ServerEnvelope) {
        if self.events.try_send(envelope).is_err() {
            // A peer that misses membership events cannot safely demux media.
            self.connection
                .close(2_u32.into(), b"control consumer too slow");
        }
    }

    fn voice_state(&self, conn: Id) -> Msg {
        Msg::VoiceStateUpdated(v1::VoiceStateUpdated {
            user_id: id_to_bytes(self.user),
            conn_id: id_to_bytes(conn),
            channel_id: self.membership.map(|m| id_to_bytes(m.channel)),
            self_mute: self.membership.is_some_and(|m| m.muted),
            self_deaf: self.membership.is_some_and(|m| m.deaf),
            server_mute: false,
        })
    }

    fn track(&self, conn: Id, member: Membership) -> Msg {
        Msg::TrackAdded(v1::TrackAdded {
            track_id: member.track,
            user_id: id_to_bytes(self.user),
            conn_id: id_to_bytes(conn),
            kind: v1::TrackKind::Audio.into(),
            codec: Some(v1::Codec {
                name: "opus".into(),
                sample_rate: 48_000,
                channels: 1,
                description: bytes::Bytes::new(),
            }),
            channel_id: id_to_bytes(member.channel),
        })
    }
}

impl Registry {
    pub(crate) fn register(
        &self,
        conn: Id,
        user: Id,
        connection: quinn::Connection,
        events: mpsc::Sender<v1::ServerEnvelope>,
    ) {
        self.state.lock().peers.insert(
            conn,
            Peer {
                user,
                connection,
                events,
                membership: None,
            },
        );
    }

    pub(crate) fn reply(&self, conn: Id, envelope: v1::ServerEnvelope) {
        if let Some(peer) = self.state.lock().peers.get(&conn) {
            peer.send(envelope);
        }
    }

    /// Join or switch channels. Existing tracks are announced to the newcomer,
    /// including its own track assignment. Rejoining updates mute/deaf in place.
    pub fn join(&self, conn: Id, channel: Id, muted: bool, deaf: bool) -> Result<(), Error> {
        let mut state = self.state.lock();
        let old = state
            .peers
            .get(&conn)
            .ok_or(Error::UnknownConnection)?
            .membership;
        if old.is_some_and(|m| m.channel == channel) {
            state.set_flags(conn, muted, deaf);
            return Ok(());
        }
        let track = state
            .next_track
            .checked_add(1)
            .filter(|v| *v <= ss_net::media::MAX_TRACK_ID)
            .ok_or(Error::TracksExhausted)?;
        state.next_track = track;
        state.leave(conn);
        let membership = Membership {
            channel,
            track,
            muted,
            deaf,
        };
        state
            .peers
            .get_mut(&conn)
            .ok_or(Error::UnknownConnection)?
            .membership = Some(membership);
        let peer = &state.peers[&conn];
        if let Some(members) = state.channels.get(&channel) {
            for id in members {
                let existing = &state.peers[id];
                if let Some(member) = existing.membership {
                    peer.emit(existing.track(*id, member));
                    peer.emit(existing.voice_state(*id));
                }
            }
        }
        state.channels.entry(channel).or_default().insert(conn);
        let peer = &state.peers[&conn];
        let track_event = peer.track(conn, membership);
        let voice_event = peer.voice_state(conn);
        state.broadcast(channel, &track_event);
        state.broadcast(channel, &voice_event);
        Ok(())
    }

    /// Leave voice, preserving the control session. Idempotent.
    pub fn leave(&self, conn: Id) {
        self.state.lock().leave(conn);
    }

    /// Apply local mute/deaf flags to the current membership, if any.
    pub fn set_flags(&self, conn: Id, muted: bool, deaf: bool) {
        self.state.lock().set_flags(conn, muted, deaf);
    }

    pub(crate) fn unregister(&self, conn: Id) {
        let mut state = self.state.lock();
        state.leave(conn);
        state.peers.remove(&conn);
    }

    /// Number of connected peers that completed Hello.
    pub fn connection_count(&self) -> usize {
        self.state.lock().peers.len()
    }
}

impl State {
    fn broadcast(&self, channel: Id, event: &Msg) {
        if let Some(members) = self.channels.get(&channel) {
            for id in members {
                self.peers[id].emit(event.clone());
            }
        }
    }

    fn leave(&mut self, conn: Id) {
        let Some(peer) = self.peers.get_mut(&conn) else {
            return;
        };
        let Some(member) = peer.membership.take() else {
            return;
        };
        let voice_event = peer.voice_state(conn);
        self.broadcast(
            member.channel,
            &Msg::TrackRemoved(v1::TrackRemoved {
                track_id: member.track,
            }),
        );
        self.broadcast(member.channel, &voice_event);
        if let Some(members) = self.channels.get_mut(&member.channel) {
            members.remove(&conn);
            if members.is_empty() {
                self.channels.remove(&member.channel);
            }
        }
    }

    fn set_flags(&mut self, conn: Id, muted: bool, deaf: bool) {
        let Some(peer) = self.peers.get_mut(&conn) else {
            return;
        };
        let Some(member) = peer.membership.as_mut() else {
            return;
        };
        member.muted = muted;
        member.deaf = deaf;
        let channel = member.channel;
        let event = peer.voice_state(conn);
        self.broadcast(channel, &event);
    }
}
