//! Read-only shared state, updated in control-stream order by the client worker.

mod messages;
use crate::{ClientEvent, ConnectionState, Error};
pub use messages::MessageCache;
use parking_lot::{RwLock, RwLockReadGuard};
use ss_core::{Id, Permissions};
use ss_proto::{
    v1::{self, server_envelope::Msg},
    validate::id_from_bytes,
};
use std::{collections::BTreeMap, sync::Arc};

/// A consistent view of one server. Maps use validated ULIDs; voice uses the
/// connection id because a member can have several simultaneous sessions.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Current lifecycle. Old state is retained during reconnect, then replaced.
    pub connection: ConnectionState,
    /// Public server description.
    pub server: Option<v1::ServerInfo>,
    /// Authenticated profile.
    pub own_user: Option<v1::User>,
    /// Guild channels keyed by id. Use `channel_tree` for display order.
    pub channels: BTreeMap<Id, v1::Channel>,
    /// Direct and group conversations.
    pub direct_messages: BTreeMap<Id, v1::Channel>,
    /// Roles keyed by id.
    pub roles: BTreeMap<Id, v1::Role>,
    /// Members, including current presence and role ids.
    pub members: BTreeMap<Id, v1::Member>,
    /// Active voice memberships keyed by connection id.
    pub voice_states: BTreeMap<Id, v1::VoiceState>,
    /// Media track descriptions.
    pub tracks: BTreeMap<u32, v1::Track>,
    /// Own read cursors.
    pub read_states: BTreeMap<Id, v1::ReadState>,
    /// Recent messages plus explicitly loaded historical pages.
    pub messages: BTreeMap<Id, MessageCache>,
    /// Invites observed since connect, for authorized users.
    pub invites: BTreeMap<Id, v1::Invite>,
    /// Session generation, advanced only when a Welcome snapshot is installed.
    pub generation: u64,
    /// Monotonic local revision; protects HTTP pages against concurrent events.
    pub revision: u64,
}

impl State {
    /// Depth-first channel tree, siblings sorted by (position, id). Missing
    /// parents and cycles cannot hide a visible channel or recurse forever.
    pub fn channel_tree(&self) -> Vec<&v1::Channel> {
        let mut children: BTreeMap<Option<Id>, Vec<(Id, &v1::Channel)>> = BTreeMap::new();
        for (&key, channel) in &self.channels {
            let parent = channel
                .parent_id
                .as_deref()
                .and_then(|raw| id(raw).ok())
                .filter(|parent| self.channels.contains_key(parent));
            children.entry(parent).or_default().push((key, channel));
        }
        for siblings in children.values_mut() {
            siblings.sort_by_key(|(key, channel)| (channel.position, *key));
        }
        let mut stack: Vec<_> = children
            .get(&None)
            .into_iter()
            .flatten()
            .rev()
            .copied()
            .collect();
        let mut visited = std::collections::BTreeSet::new();
        let mut result = Vec::new();
        loop {
            if let Some((key, channel)) = stack.pop() {
                if !visited.insert(key) {
                    continue;
                }
                result.push(channel);
                stack.extend(
                    children
                        .get(&Some(key))
                        .into_iter()
                        .flatten()
                        .rev()
                        .copied(),
                );
            } else if let Some((&key, channel)) =
                self.channels.iter().find(|(key, _)| !visited.contains(key))
            {
                stack.push((key, channel));
            } else {
                break;
            }
        }
        result
    }

    /// Server-resolved permissions. None means an older server did not supply
    /// them; never guess by ignoring channel overrides.
    pub fn permissions(&self, channel: Id) -> Option<Permissions> {
        self.channels
            .get(&channel)
            .or_else(|| self.direct_messages.get(&channel))?
            .effective_permissions
            .and_then(Permissions::from_bits)
    }
}

/// Cloneable read handle. Callers cannot mutate the mirror.
#[derive(Clone, Default)]
pub struct StateStore(Arc<RwLock<State>>);

impl StateStore {
    /// Borrow the current snapshot. Drop the guard before awaiting anything.
    pub fn read(&self) -> RwLockReadGuard<'_, State> {
        self.0.read()
    }
    /// Owned copy for hosts that cannot retain a read guard.
    pub fn snapshot(&self) -> State {
        self.0.read().clone()
    }
    pub(crate) fn connection(&self, value: ConnectionState) {
        self.0.write().connection = value;
    }

    pub(crate) fn welcome(&self, welcome: &v1::Welcome) -> Result<(), Error> {
        self.install(welcome, ConnectionState::Connected)
    }

    pub(crate) fn install(
        &self,
        welcome: &v1::Welcome,
        connection: ConnectionState,
    ) -> Result<(), Error> {
        let snapshot = welcome
            .snapshot
            .as_ref()
            .ok_or(Error::Protocol("missing snapshot"))?;
        let user = welcome
            .user
            .as_ref()
            .ok_or(Error::Protocol("missing own user"))?;
        let _ = id(&user.id)?;
        let mut next = State {
            server: welcome.server_info.clone(),
            own_user: Some(user.clone()),
            connection,
            ..State::default()
        };
        for channel in snapshot.channels.iter().chain(&snapshot.direct_messages) {
            next.channel(channel.clone())?;
        }
        for role in &snapshot.roles {
            next.roles.insert(id(&role.id)?, role.clone());
        }
        for member in &snapshot.members {
            next.member(member.clone())?;
        }
        for voice in &snapshot.voice_states {
            next.voice_states.insert(id(&voice.conn_id)?, voice.clone());
        }
        for track in &snapshot.tracks {
            next.tracks.insert(track.track_id, track.clone());
        }
        for read in &snapshot.read_states {
            next.read_states.insert(id(&read.channel_id)?, read.clone());
        }
        let mut state = self.0.write();
        next.messages = std::mem::take(&mut state.messages);
        next.messages.retain(|channel, _| {
            next.channels.contains_key(channel) || next.direct_messages.contains_key(channel)
        });
        next.revision = state.revision.wrapping_add(1);
        next.generation = state.generation.wrapping_add(1);
        *state = next;
        Ok(())
    }

    pub(crate) fn apply(&self, message: Msg) -> Result<ClientEvent, Error> {
        let mut state = self.0.write();
        let event = state.apply(message)?;
        state.revision = state.revision.wrapping_add(1);
        Ok(event)
    }

    pub(crate) fn history(
        &self,
        channel: Id,
        revision: u64,
        page: Vec<v1::Message>,
    ) -> Result<(), Error> {
        let mut state = self.0.write();
        if state.revision != revision || state.connection != ConnectionState::Connected {
            return Err(Error::StateChanged);
        }
        if !state.channels.contains_key(&channel) && !state.direct_messages.contains_key(&channel) {
            return Err(Error::StateChanged);
        }
        state.messages.entry(channel).or_default().load(page)?;
        state.revision = state.revision.wrapping_add(1);
        Ok(())
    }

    pub(crate) fn message_cursors(&self) -> Vec<(Id, Id)> {
        self.0
            .read()
            .messages
            .iter()
            .filter_map(|(&channel, cache)| {
                cache
                    .newest()
                    .and_then(|message| id(&message.id).ok())
                    .map(|cursor| (channel, cursor))
            })
            .collect()
    }

    pub(crate) fn resync(&self, channel: Id, page: Vec<v1::Message>) -> Result<(), Error> {
        let mut state = self.0.write();
        if !matches!(state.connection, ConnectionState::Reconnecting { .. })
            || (!state.channels.contains_key(&channel)
                && !state.direct_messages.contains_key(&channel))
        {
            return Err(Error::StateChanged);
        }
        state.messages.entry(channel).or_default().load(page)?;
        state.revision = state.revision.wrapping_add(1);
        Ok(())
    }
}

pub(crate) fn id(bytes: &[u8]) -> Result<Id, Error> {
    id_from_bytes(bytes).map_err(|_| Error::Protocol("invalid entity id"))
}

impl State {
    fn channel(&mut self, channel: v1::Channel) -> Result<(), Error> {
        let key = id(&channel.id)?;
        if let Some(bits) = channel.effective_permissions
            && Permissions::from_bits(bits).is_none()
        {
            return Err(Error::Protocol("invalid permissions"));
        }
        self.channels.remove(&key);
        self.direct_messages.remove(&key);
        if matches!(
            v1::ChannelKind::try_from(channel.kind),
            Ok(v1::ChannelKind::Dm | v1::ChannelKind::GroupDm)
        ) {
            self.direct_messages.insert(key, channel);
        } else {
            self.channels.insert(key, channel);
        }
        Ok(())
    }
    fn member(&mut self, member: v1::Member) -> Result<(), Error> {
        let user = member
            .user
            .as_ref()
            .ok_or(Error::Protocol("missing member profile"))?;
        let key = id(&user.id)?;
        if self.own_user.as_ref().is_some_and(|own| own.id == user.id) {
            self.own_user = Some(user.clone());
        }
        self.members.insert(key, member);
        Ok(())
    }
    fn message(
        &mut self,
        message: Option<v1::Message>,
        deleted: bool,
        created: bool,
    ) -> Result<ClientEvent, Error> {
        let message = message.ok_or(Error::Protocol("missing message"))?;
        let channel = id(&message.channel_id)?;
        let key = id(&message.id)?;
        if !self.channels.contains_key(&channel) && !self.direct_messages.contains_key(&channel) {
            return Ok(ClientEvent::MessagesChanged { channel });
        }
        let cache = self.messages.entry(channel).or_default();
        if deleted {
            cache.remove(key);
        } else {
            cache.upsert(message.clone(), created)?;
        }
        if let Some(dm) = self.direct_messages.get_mut(&channel) {
            if deleted && dm.last_message_id.as_ref() == Some(&message.id) {
                dm.last_message_id = cache.newest().map(|m| m.id.clone());
            } else if created
                && dm
                    .last_message_id
                    .as_ref()
                    .is_none_or(|last| last < &message.id)
            {
                dm.last_message_id = Some(message.id);
            }
        }
        Ok(ClientEvent::MessagesChanged { channel })
    }

    #[allow(clippy::too_many_lines, reason = "exhaustive protocol event reducer")]
    fn apply(&mut self, message: Msg) -> Result<ClientEvent, Error> {
        use ClientEvent as E;
        Ok(match message {
            Msg::ChannelCreated(event) => {
                self.channel(event.channel.ok_or(Error::Protocol("missing channel"))?)?;
                E::ChannelsChanged
            }
            Msg::ChannelUpdated(event) => {
                self.channel(event.channel.ok_or(Error::Protocol("missing channel"))?)?;
                E::ChannelsChanged
            }
            Msg::ChannelDeleted(event) => {
                let key = id(&event.channel.ok_or(Error::Protocol("missing channel"))?.id)?;
                self.channels.remove(&key);
                self.direct_messages.remove(&key);
                self.messages.remove(&key);
                self.read_states.remove(&key);
                self.voice_states.retain(|_, voice| {
                    voice.channel_id.as_deref().and_then(|raw| id(raw).ok()) != Some(key)
                });
                self.tracks
                    .retain(|_, track| id(&track.channel_id).ok() != Some(key));
                E::ChannelsChanged
            }
            Msg::MemberJoined(event) => {
                self.member(event.member.ok_or(Error::Protocol("missing member"))?)?;
                E::MembersChanged
            }
            Msg::MemberUpdated(event) => {
                self.member(event.member.ok_or(Error::Protocol("missing member"))?)?;
                E::MembersChanged
            }
            Msg::MemberLeft(event) => {
                let user = event
                    .member
                    .and_then(|m| m.user)
                    .ok_or(Error::Protocol("missing member"))?;
                self.members.remove(&id(&user.id)?);
                self.voice_states
                    .retain(|_, voice| voice.user_id != user.id);
                self.tracks.retain(|_, track| track.user_id != user.id);
                E::MembersChanged
            }
            Msg::PresenceUpdated(event) => {
                if let Some(member) = self.members.get_mut(&id(&event.user_id)?) {
                    member.presence = Some(v1::Presence {
                        status: event.status,
                        activity: event.activity,
                    });
                }
                E::MembersChanged
            }
            Msg::MemberRolesUpdated(event) => {
                if let Some(member) = self.members.get_mut(&id(&event.user_id)?) {
                    member.roles = event.role_ids;
                }
                E::MembersChanged
            }
            Msg::RoleCreated(event) => {
                let role = event.role.ok_or(Error::Protocol("missing role"))?;
                self.roles.insert(id(&role.id)?, role);
                E::MembersChanged
            }
            Msg::RoleUpdated(event) => {
                let role = event.role.ok_or(Error::Protocol("missing role"))?;
                self.roles.insert(id(&role.id)?, role);
                E::MembersChanged
            }
            Msg::RoleDeleted(event) => {
                let role = event.role.ok_or(Error::Protocol("missing role"))?;
                self.roles.remove(&id(&role.id)?);
                for member in self.members.values_mut() {
                    member.roles.retain(|key| key != &role.id);
                }
                E::MembersChanged
            }
            Msg::VoiceStateUpdated(event) => {
                let key = id(&event.conn_id)?;
                if event.channel_id.is_none() {
                    self.voice_states.remove(&key);
                    self.tracks
                        .retain(|_, track| track.conn_id != event.conn_id);
                } else {
                    self.voice_states.insert(
                        key,
                        v1::VoiceState {
                            user_id: event.user_id,
                            conn_id: event.conn_id,
                            channel_id: event.channel_id,
                            self_mute: event.self_mute,
                            self_deaf: event.self_deaf,
                            server_mute: event.server_mute,
                        },
                    );
                }
                E::VoiceChanged
            }
            Msg::TrackAdded(event) => {
                self.tracks.insert(
                    event.track_id,
                    v1::Track {
                        track_id: event.track_id,
                        user_id: event.user_id,
                        conn_id: event.conn_id,
                        kind: event.kind,
                        codec: event.codec,
                        channel_id: event.channel_id,
                    },
                );
                E::VoiceChanged
            }
            Msg::TrackRemoved(event) => {
                self.tracks.remove(&event.track_id);
                E::VoiceChanged
            }
            Msg::MessageCreated(event) => return self.message(event.message, false, true),
            Msg::MessageUpdated(event) => return self.message(event.message, false, false),
            Msg::MessageDeleted(event) => return self.message(event.message, true, false),
            Msg::ReadStateUpdated(event) => {
                self.read_states.insert(
                    id(&event.channel_id)?,
                    v1::ReadState {
                        channel_id: event.channel_id,
                        last_read_message_id: event.last_read_message_id,
                        mention_count: event.mention_count,
                    },
                );
                E::ReadStatesChanged
            }
            Msg::TypingStarted(event) => E::Typing {
                channel: id(&event.channel_id)?,
                user: id(&event.user_id)?,
            },
            Msg::InviteCreated(event) => {
                let invite = event.invite.ok_or(Error::Protocol("missing invite"))?;
                self.invites.insert(id(&invite.id)?, invite);
                E::InvitesChanged
            }
            Msg::InviteRevoked(event) => {
                self.invites.remove(&id(&event
                    .invite
                    .ok_or(Error::Protocol("missing invite"))?
                    .id)?);
                E::InvitesChanged
            }
            Msg::Kicked(event) => E::Removed {
                banned: false,
                reason: event.reason,
            },
            Msg::Banned(event) => E::Removed {
                banned: true,
                reason: event.reason,
            },
            Msg::ServerNotice(event) => E::Notice(event.text),
            Msg::Error(error) => E::ServerError(error),
            Msg::RequestKeyframe(event) => E::KeyframeRequested {
                track: event.track_id,
            },
            Msg::SubscriberHint(event) => E::SubscriberHint {
                track: event.track_id,
                max_height: event.max_height,
                max_fps: event.max_fps,
                viewer_count: event.viewer_count,
            },
            Msg::Welcome(_) | Msg::Ack(_) | Msg::Pong(_) => {
                return Err(Error::Protocol("unsolicited response"));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ss_proto::validate::id_to_bytes as wire;

    fn channel(key: Id, position: i32) -> v1::Channel {
        v1::Channel {
            id: wire(key),
            kind: v1::ChannelKind::Text.into(),
            position,
            effective_permissions: Some(Permissions::EVERYONE.bits()),
            ..Default::default()
        }
    }
    fn message(key: Id, channel: Id, content: &str) -> v1::Message {
        v1::Message {
            id: wire(key),
            channel_id: wire(channel),
            content: content.into(),
            ..Default::default()
        }
    }
    fn store() -> (StateStore, Id, Id) {
        let user = Id::generate();
        let key = Id::generate();
        let store = StateStore::default();
        store
            .welcome(&v1::Welcome {
                user: Some(v1::User {
                    id: wire(user),
                    ..Default::default()
                }),
                snapshot: Some(v1::Snapshot {
                    channels: vec![channel(key, 0)],
                    members: vec![v1::Member {
                        user: Some(v1::User {
                            id: wire(user),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            })
            .unwrap();
        (store, key, user)
    }
    #[test]
    fn recent_ring_history_edits_deletes_and_echoes_share_identity() {
        let (store, channel, _) = store();
        let keys: Vec<_> = (0..205).map(|_| Id::generate()).collect();
        // ULIDs created in one millisecond need not be generated in sort order.
        let mut keys = keys;
        keys.sort();
        for key in &keys {
            store
                .apply(Msg::MessageCreated(v1::MessageCreated {
                    message: Some(message(*key, channel, "first")),
                    nonce: None,
                }))
                .unwrap();
        }
        assert_eq!(store.read().messages[&channel].recent_len(), 200);
        let revision = store.read().revision;
        store
            .history(
                channel,
                revision,
                keys[..10]
                    .iter()
                    .map(|key| message(*key, channel, "history"))
                    .collect(),
            )
            .unwrap();
        assert_eq!(store.read().messages[&channel].ordered().len(), 205);
        let first = keys[0];
        let last = keys[204];
        for key in [first, last] {
            store
                .apply(Msg::MessageUpdated(v1::MessageUpdated {
                    message: Some(message(key, channel, "edited")),
                    nonce: None,
                }))
                .unwrap();
        }
        store
            .apply(Msg::MessageCreated(v1::MessageCreated {
                message: Some(message(last, channel, "echo")),
                nonce: Some("dedupe".into()),
            }))
            .unwrap();
        assert_eq!(store.read().messages[&channel].ordered().len(), 205);
        assert_eq!(
            store.read().messages[&channel].ordered()[0].content,
            "edited"
        );
        store
            .apply(Msg::MessageDeleted(v1::MessageDeleted {
                message: Some(message(first, channel, "")),
                nonce: None,
            }))
            .unwrap();
        assert_eq!(store.read().messages[&channel].ordered().len(), 204);
        assert!(matches!(
            store.history(channel, revision, vec![message(first, channel, "stale")]),
            Err(Error::StateChanged)
        ));
    }
    #[test]
    fn visibility_revocation_purges_private_cache_and_media() {
        let (store, channel, user) = store();
        let conn = Id::generate();
        store
            .apply(Msg::MessageCreated(v1::MessageCreated {
                message: Some(message(Id::generate(), channel, "private")),
                nonce: None,
            }))
            .unwrap();
        store
            .apply(Msg::VoiceStateUpdated(v1::VoiceStateUpdated {
                user_id: wire(user),
                conn_id: wire(conn),
                channel_id: Some(wire(channel)),
                ..Default::default()
            }))
            .unwrap();
        store
            .apply(Msg::TrackAdded(v1::TrackAdded {
                track_id: 7,
                conn_id: wire(conn),
                user_id: wire(user),
                channel_id: wire(channel),
                ..Default::default()
            }))
            .unwrap();
        store
            .apply(Msg::ReadStateUpdated(v1::ReadStateUpdated {
                channel_id: wire(channel),
                last_read_message_id: wire(Id::generate()),
                mention_count: 2,
            }))
            .unwrap();
        store
            .apply(Msg::ChannelDeleted(v1::ChannelDeleted {
                channel: Some(v1::Channel {
                    id: wire(channel),
                    ..Default::default()
                }),
            }))
            .unwrap();
        let state = store.read();
        assert!(
            state.channels.is_empty()
                && state.messages.is_empty()
                && state.tracks.is_empty()
                && state.voice_states.is_empty()
                && state.read_states.is_empty()
        );
    }
    #[test]
    fn tree_order_is_depth_first_and_malformed_cycles_cannot_loop() {
        let (store, root, _) = store();
        let child = Id::generate();
        let sibling = Id::generate();
        let mut c = channel(child, 99);
        c.parent_id = Some(wire(root));
        store
            .apply(Msg::ChannelCreated(v1::ChannelCreated { channel: Some(c) }))
            .unwrap();
        store
            .apply(Msg::ChannelCreated(v1::ChannelCreated {
                channel: Some(channel(sibling, 1)),
            }))
            .unwrap();
        assert_eq!(
            store
                .read()
                .channel_tree()
                .iter()
                .map(|c| id(&c.id).unwrap())
                .collect::<Vec<_>>(),
            [root, child, sibling]
        );
        let mut c = channel(root, 0);
        c.parent_id = Some(wire(child));
        store
            .apply(Msg::ChannelUpdated(v1::ChannelUpdated { channel: Some(c) }))
            .unwrap();
        assert_eq!(store.read().channel_tree().len(), 3);
    }
    #[test]
    fn snapshot_replaces_stale_state_and_missing_permissions_stay_unknown() {
        let (store, key, _) = store();
        assert_eq!(store.read().permissions(key), Some(Permissions::EVERYONE));
        let mut c = channel(key, 0);
        c.effective_permissions = None;
        store
            .apply(Msg::ChannelUpdated(v1::ChannelUpdated { channel: Some(c) }))
            .unwrap();
        assert_eq!(store.read().permissions(key), None);
        let mut welcome = v1::Welcome {
            user: store.read().own_user.clone(),
            snapshot: Some(v1::Snapshot::default()),
            ..Default::default()
        };
        store.welcome(&welcome).unwrap();
        assert!(store.read().channels.is_empty());
        welcome.user = None;
        let before = store.read().revision;
        assert!(store.welcome(&welcome).is_err());
        assert_eq!(store.read().revision, before);
    }
    #[test]
    fn reconnect_snapshot_keeps_visible_message_cursors_and_merges_deltas() {
        let (store, channel_id, _) = store();
        let first = Id::generate();
        let second = Id::generate();
        let (first, second) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        store
            .apply(Msg::MessageCreated(v1::MessageCreated {
                message: Some(message(first, channel_id, "before")),
                nonce: None,
            }))
            .unwrap();
        let welcome = v1::Welcome {
            user: store.read().own_user.clone(),
            snapshot: Some(v1::Snapshot {
                channels: vec![channel(channel_id, 0)],
                ..Default::default()
            }),
            ..Default::default()
        };
        store
            .install(
                &welcome,
                ConnectionState::Reconnecting {
                    attempt: 1,
                    delay: std::time::Duration::from_secs(2),
                },
            )
            .unwrap();
        assert_eq!(store.message_cursors(), vec![(channel_id, first)]);
        store
            .resync(channel_id, vec![message(second, channel_id, "after")])
            .unwrap();
        assert_eq!(
            store.read().messages[&channel_id]
                .ordered()
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            ["before", "after"]
        );
    }
    #[test]
    fn own_profile_presence_roles_and_voice_sessions_reduce_independently() {
        let (store, channel, user) = store();
        let role = Id::generate();
        let conn1 = Id::generate();
        let conn2 = Id::generate();
        store
            .apply(Msg::RoleCreated(v1::RoleCreated {
                role: Some(v1::Role {
                    id: wire(role),
                    ..Default::default()
                }),
            }))
            .unwrap();
        store
            .apply(Msg::MemberRolesUpdated(v1::MemberRolesUpdated {
                user_id: wire(user),
                role_ids: vec![wire(role)],
            }))
            .unwrap();
        store
            .apply(Msg::PresenceUpdated(v1::PresenceUpdated {
                user_id: wire(user),
                status: v1::PresenceStatus::Idle.into(),
                activity: "reading".into(),
            }))
            .unwrap();
        assert_eq!(
            store.read().members[&user]
                .presence
                .as_ref()
                .unwrap()
                .activity,
            "reading"
        );
        for conn in [conn1, conn2] {
            store
                .apply(Msg::VoiceStateUpdated(v1::VoiceStateUpdated {
                    user_id: wire(user),
                    conn_id: wire(conn),
                    channel_id: Some(wire(channel)),
                    ..Default::default()
                }))
                .unwrap();
        }
        store
            .apply(Msg::VoiceStateUpdated(v1::VoiceStateUpdated {
                user_id: wire(user),
                conn_id: wire(conn1),
                ..Default::default()
            }))
            .unwrap();
        assert_eq!(store.read().voice_states.len(), 1);
        assert!(store.read().voice_states.contains_key(&conn2));
        store
            .apply(Msg::RoleDeleted(v1::RoleDeleted {
                role: Some(v1::Role {
                    id: wire(role),
                    ..Default::default()
                }),
            }))
            .unwrap();
        assert_eq!(store.read().members[&user].roles, [] as [bytes::Bytes; 0]);
        let member = v1::Member {
            user: Some(v1::User {
                id: wire(user),
                display_name: "New".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        store
            .apply(Msg::MemberUpdated(v1::MemberUpdated {
                member: Some(member),
            }))
            .unwrap();
        assert_eq!(store.read().own_user.as_ref().unwrap().display_name, "New");
    }
}
