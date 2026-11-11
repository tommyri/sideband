//! Notification policy, derived from the mirrored server state and the
//! user's saved preferences.
//!
//! Decides whether a new message alerts, which cue plays, what a desktop
//! notification says and what the badge counts. Hosts show notifications,
//! set badges and play sounds; nothing here touches a platform API.

use crate::{ClientEvent, ConnectionState, State, v1, voice::Cue};
use serde::{Deserialize, Serialize};
use ss_core::{Id, Permissions};
use std::collections::{BTreeMap, BTreeSet};

/// How much one channel or conversation notifies.
#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    /// Every message.
    All,
    /// Mentions and direct messages.
    #[default]
    Mentions,
    /// Nothing.
    None,
}

/// Saved notification preferences. Holds no message contents.
///
/// The serde shape is the desktop's saved `sessions.json` format.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    /// Show desktop notifications.
    pub desktop: bool,
    /// Show message text in desktop notifications.
    pub previews: bool,
    /// Do not disturb: no alerts, sounds or notifications anywhere.
    pub dnd: bool,
    /// Servers that never alert or play sounds.
    pub muted_servers: BTreeSet<String>,
    /// Per-channel levels keyed by `"{server}/{channel}"`.
    pub channels: BTreeMap<String, Level>,
}

impl Preferences {
    /// Most channel levels a saved set may hold.
    pub const MAX_CHANNELS: usize = 10_000;
    /// Most muted servers a saved set may hold.
    pub const MAX_MUTED_SERVERS: usize = 1000;
    /// Longest channel key a saved set may hold, in bytes.
    pub const MAX_KEY_LEN: usize = 512;

    /// Whether a message in `channel` on `server` alerts. Do not disturb and a
    /// muted server win over the channel's level.
    pub fn allows(&self, server: &str, channel: &str, mentioned: bool, dm: bool) -> bool {
        if self.dnd || self.muted_servers.contains(server) {
            return false;
        }
        match self
            .channels
            .get(&format!("{server}/{channel}"))
            .copied()
            .unwrap_or_default()
        {
            Level::All => true,
            Level::Mentions => mentioned || dm,
            Level::None => false,
        }
    }

    /// Whether cues for `server` may play.
    pub fn sounds(&self, server: &str) -> bool {
        !self.dnd && !self.muted_servers.contains(server)
    }

    /// Whether desktop notifications are on and not silenced by do not disturb.
    pub fn desktop_enabled(&self) -> bool {
        self.desktop && !self.dnd
    }

    /// Whether the set is small enough to save.
    pub fn within_limits(&self) -> bool {
        self.channels.len() <= Self::MAX_CHANNELS
            && self.muted_servers.len() <= Self::MAX_MUTED_SERVERS
            && self.channels.keys().all(|k| k.len() <= Self::MAX_KEY_LEN)
    }
}

/// Whether the user can currently open `channel`: connected, and allowed to
/// view it. Alerts, badge counts and notification activation all require it.
pub fn can_open(state: &State, channel: Id) -> bool {
    state.connection == ConnectionState::Connected
        && state
            .permissions(channel)
            .is_some_and(|p| p.contains(Permissions::VIEW_CHANNEL))
}

/// Unread mentions on this server, over the channels the user can open.
/// Zero while disconnected.
pub fn mention_count(state: &State) -> i64 {
    state
        .read_states
        .iter()
        .filter(|(channel, _)| can_open(state, **channel))
        .map(|(_, read)| i64::from(read.mention_count))
        .sum()
}

/// The presence to send for the do-not-disturb setting, keeping the user's
/// current activity text.
pub fn presence(state: &State, dnd: bool) -> v1::SetPresence {
    let activity = state
        .own_user
        .as_ref()
        .and_then(|own| Id::try_from(own.id.as_ref()).ok())
        .and_then(|id| state.members.get(&id))
        .and_then(|m| m.presence.as_ref())
        .map(|p| p.activity.clone())
        .unwrap_or_default();
    v1::SetPresence {
        status: if dnd {
            v1::PresenceStatus::Dnd
        } else {
            v1::PresenceStatus::Online
        } as i32,
        activity,
    }
}

/// The presence to send again after `event`: a resync under do not disturb
/// re-asserts it, because the server reset presence when the connection
/// dropped.
pub fn presence_after(event: &ClientEvent, prefs: &Preferences) -> Option<v1::SetPresence> {
    (matches!(event, ClientEvent::Resynced) && prefs.dnd).then(|| v1::SetPresence {
        status: v1::PresenceStatus::Dnd as i32,
        activity: String::new(),
    })
}

/// Desktop notification text with previews off.
pub const DIRECT_MESSAGE_BODY: &str = "You received a direct message.";
/// Desktop notification text with previews off.
pub const CHANNEL_MESSAGE_BODY: &str = "You have a new channel message.";
/// Longest preview, in characters.
pub const PREVIEW_CHARS: usize = 180;

/// What a host does for one new message.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Alert {
    /// The cue to play.
    pub cue: Cue,
    /// The desktop notification body, when one should be shown.
    pub desktop: Option<String>,
}

/// The alert for a new message, from the current cache rather than the
/// event, so a queued event that lags an edit, deletion or visibility change
/// never exposes the earlier body. `None` when it must not alert: the channel
/// cannot be opened, the message is gone, the user wrote it, or the
/// preferences say no. `focused` is whether the host's window has focus,
/// which keeps the cue but drops the desktop notification.
pub fn alert(
    state: &State,
    prefs: &Preferences,
    server: &str,
    channel: Id,
    message: Id,
    focused: bool,
) -> Option<Alert> {
    if !can_open(state, channel) {
        return None;
    }
    let message = state
        .messages
        .get(&channel)?
        .ordered()
        .into_iter()
        .find(|m| Id::try_from(m.id.as_ref()).ok() == Some(message))?;
    let own = message.author.as_ref().is_some_and(|author| {
        state
            .own_user
            .as_ref()
            .is_some_and(|own| own.id == author.id)
    });
    if own {
        return None;
    }
    let mentioned = state.mentions_me(message);
    let dm = state.direct_messages.contains_key(&channel);
    if !prefs.allows(server, &channel.to_string(), mentioned, dm) {
        return None;
    }
    Some(Alert {
        cue: if dm { Cue::DirectMessage } else { Cue::Mention },
        desktop: (prefs.desktop && !focused).then(|| body(prefs, message, dm)),
    })
}

fn body(prefs: &Preferences, message: &v1::Message, dm: bool) -> String {
    if prefs.previews {
        message
            .content
            .chars()
            .filter(|c| !c.is_control())
            .take(PREVIEW_CHARS)
            .collect()
    } else if dm {
        DIRECT_MESSAGE_BODY.into()
    } else {
        CHANNEL_MESSAGE_BODY.into()
    }
}

/// Notification state a host keeps across events for every connected
/// server: which messages already alerted, each server's mention count and
/// who was in the call last time.
#[derive(Default, Debug)]
pub struct Tracker {
    seen: BTreeSet<(String, Id)>,
    counts: BTreeMap<String, i64>,
    calls: BTreeMap<String, (Id, BTreeSet<Id>)>,
}

impl Tracker {
    /// Message ids remembered for deduplication, across servers.
    pub const SEEN: usize = 512;

    /// Whether a received message is new and in a channel the user can open.
    /// Records it, so a cached echo or snapshot overlap of the same message
    /// is refused later. A message in a channel the user cannot open is not
    /// recorded.
    pub fn first_sighting(
        &mut self,
        state: &State,
        server: &str,
        channel: Id,
        message: Id,
    ) -> bool {
        if !can_open(state, channel) || !self.seen.insert((server.into(), message)) {
            return false;
        }
        if self.seen.len() > Self::SEEN {
            self.seen.pop_first();
        }
        true
    }

    /// Record this server's mention count and return the badge for every
    /// server: the total, or `None` when there is nothing to show.
    pub fn badge(&mut self, server: &str, state: &State) -> Option<i64> {
        self.counts.insert(server.to_owned(), mention_count(state));
        let total = self.counts.values().sum::<i64>();
        (total > 0).then_some(total)
    }

    /// The join, leave, lost or restored cue for the user's call on `server`
    /// after `event`. `call` is the voice channel the user is in on this
    /// server, or `None` when they are not in a call here.
    ///
    /// Join and leave compare who is in the call with the previous event, so
    /// the first event after joining or moving channel plays nothing.
    pub fn voice_cue(
        &mut self,
        server: &str,
        call: Option<Id>,
        state: &State,
        event: &ClientEvent,
        prefs: &Preferences,
    ) -> Option<Cue> {
        let Some(call) = call else {
            self.calls.remove(server);
            return None;
        };
        let members = state
            .voice_states
            .values()
            .filter(|v| v.channel_id.as_deref().and_then(|id| Id::try_from(id).ok()) == Some(call))
            .filter_map(|v| Id::try_from(v.user_id.as_ref()).ok())
            .collect::<BTreeSet<_>>();
        let previous = self
            .calls
            .insert(server.into(), (call, members.clone()))
            .filter(|(channel, _)| *channel == call)
            .map(|(_, members)| members);
        if !prefs.sounds(server) {
            return None;
        }
        match event {
            ClientEvent::VoiceChanged => {
                let previous = previous?;
                if members.difference(&previous).next().is_some() {
                    Some(Cue::Join)
                } else if previous.difference(&members).next().is_some() {
                    Some(Cue::Leave)
                } else {
                    None
                }
            }
            ClientEvent::Connection(ConnectionState::Reconnecting { attempt: 1, .. }) => {
                Some(Cue::Lost)
            }
            ClientEvent::Resynced => Some(Cue::Restored),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StateStore;
    use ss_proto::{v1::server_envelope::Msg, validate::id_to_bytes as wire};
    use std::time::Duration;

    const SERVER: &str = "localhost:5781";

    struct Fixture {
        store: StateStore,
        own: Id,
        other: Id,
        channel: Id,
        dm: Id,
    }

    fn channel(key: Id, permissions: Permissions) -> v1::Channel {
        v1::Channel {
            id: wire(key),
            kind: v1::ChannelKind::Text.into(),
            effective_permissions: Some(permissions.bits()),
            ..Default::default()
        }
    }

    fn user(id: Id) -> v1::User {
        v1::User {
            id: wire(id),
            ..Default::default()
        }
    }

    impl Fixture {
        fn new() -> Self {
            let (own, other, channel_id, dm) = (
                Id::generate(),
                Id::generate(),
                Id::generate(),
                Id::generate(),
            );
            let store = StateStore::default();
            store
                .welcome(&v1::Welcome {
                    user: Some(user(own)),
                    snapshot: Some(v1::Snapshot {
                        channels: vec![channel(channel_id, Permissions::EVERYONE)],
                        direct_messages: vec![v1::Channel {
                            kind: v1::ChannelKind::Dm.into(),
                            member_ids: vec![wire(own), wire(other)],
                            ..channel(dm, Permissions::EVERYONE)
                        }],
                        members: vec![v1::Member {
                            user: Some(user(own)),
                            presence: Some(v1::Presence {
                                activity: "Reviewing".into(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .unwrap();
            Self {
                store,
                own,
                other,
                channel: channel_id,
                dm,
            }
        }

        /// Receive a message and return its id.
        fn receive(&self, channel: Id, author: Id, content: &str, mentions: &[Id]) -> Id {
            let id = Id::generate();
            self.store
                .apply(Msg::MessageCreated(v1::MessageCreated {
                    message: Some(v1::Message {
                        id: wire(id),
                        channel_id: wire(channel),
                        author: Some(user(author)),
                        content: content.into(),
                        mentions: mentions.iter().map(|m| wire(*m)).collect(),
                        ..Default::default()
                    }),
                    nonce: None,
                }))
                .unwrap();
            id
        }

        fn state(&self) -> State {
            self.store.snapshot()
        }
    }

    fn opted_in() -> Preferences {
        Preferences {
            desktop: true,
            ..Preferences::default()
        }
    }

    #[test]
    fn policy_honors_defaults_overrides_and_suppression() {
        let mut p = Preferences::default();
        assert!(!p.allows("s", "c", false, false));
        assert!(p.allows("s", "c", true, false));
        assert!(p.allows("s", "c", false, true));
        p.channels.insert("s/c".into(), Level::None);
        assert!(!p.allows("s", "c", true, true));
        assert!(p.allows("s", "other", true, false));
        p.channels.insert("s/c".into(), Level::All);
        assert!(p.allows("s", "c", false, false));
        assert!(p.sounds("s"));
        p.muted_servers.insert("s".into());
        assert!(!p.allows("s", "c", true, true));
        assert!(!p.sounds("s"));
        assert!(p.sounds("t") && p.allows("t", "c", true, false));
        p.muted_servers.clear();
        p.dnd = true;
        assert!(!p.allows("s", "c", true, true));
        assert!(!p.sounds("s") && !p.sounds("t"));
    }

    #[test]
    fn desktop_notifications_need_opt_in_and_no_dnd() {
        let mut p = Preferences::default();
        assert!(!p.desktop_enabled());
        p.desktop = true;
        assert!(p.desktop_enabled());
        p.dnd = true;
        assert!(!p.desktop_enabled());
    }

    #[test]
    fn oversized_preference_sets_are_refused() {
        let mut p = Preferences::default();
        p.channels
            .insert("k".repeat(Preferences::MAX_KEY_LEN), Level::All);
        assert!(p.within_limits());
        p.channels
            .insert("k".repeat(Preferences::MAX_KEY_LEN + 1), Level::All);
        assert!(!p.within_limits());
        let mut p = Preferences {
            muted_servers: (0..=Preferences::MAX_MUTED_SERVERS)
                .map(|i| i.to_string())
                .collect(),
            ..Preferences::default()
        };
        assert!(!p.within_limits());
        p.muted_servers.pop_first();
        assert!(p.within_limits());
        let p = Preferences {
            channels: (0..=Preferences::MAX_CHANNELS)
                .map(|i| (i.to_string(), Level::None))
                .collect(),
            ..Preferences::default()
        };
        assert!(!p.within_limits());
    }

    #[test]
    fn saved_preferences_keep_their_serde_shape() {
        let saved = r#"{
            "desktop": true,
            "previews": true,
            "dnd": false,
            "mutedServers": ["example.org:5781"],
            "channels": {"s/a": "all", "s/b": "mentions", "s/c": "none"}
        }"#;
        let prefs: Preferences = serde_json::from_str(saved).unwrap();
        assert!(prefs.desktop && prefs.previews && !prefs.dnd);
        assert!(prefs.muted_servers.contains("example.org:5781"));
        assert_eq!(prefs.channels["s/a"], Level::All);
        assert_eq!(prefs.channels["s/b"], Level::Mentions);
        assert_eq!(prefs.channels["s/c"], Level::None);
        assert_eq!(
            serde_json::to_value(&prefs).unwrap(),
            serde_json::from_str::<serde_json::Value>(saved).unwrap()
        );
        // Missing fields take their defaults: older files had fewer.
        let partial: Preferences = serde_json::from_str(r#"{"dnd": true}"#).unwrap();
        assert_eq!(
            partial,
            Preferences {
                dnd: true,
                ..Preferences::default()
            }
        );
        assert!(serde_json::from_str::<Level>(r#""loud""#).is_err());
    }

    #[test]
    fn mentions_and_direct_messages_alert_with_their_own_cue() {
        let f = Fixture::new();
        let mention = f.receive(f.channel, f.other, "hi", &[f.own]);
        let everyone = f.receive(f.channel, f.other, "hi @everyone", &[]);
        let plain = f.receive(f.channel, f.other, "hi", &[]);
        let direct = f.receive(f.dm, f.other, "psst", &[]);
        let state = f.state();
        let prefs = opted_in();
        let alert = |channel, message| alert(&state, &prefs, SERVER, channel, message, false);
        assert_eq!(alert(f.channel, mention).unwrap().cue, Cue::Mention);
        assert_eq!(alert(f.channel, everyone).unwrap().cue, Cue::Mention);
        assert_eq!(alert(f.channel, plain), None);
        assert_eq!(alert(f.dm, direct).unwrap().cue, Cue::DirectMessage);
        let all = Preferences {
            channels: [(format!("{SERVER}/{}", f.channel), Level::All)].into(),
            ..opted_in()
        };
        assert_eq!(
            super::alert(&state, &all, SERVER, f.channel, plain, false)
                .unwrap()
                .cue,
            Cue::Mention
        );
        let silenced = Preferences {
            channels: [(format!("{SERVER}/{}", f.dm), Level::None)].into(),
            ..opted_in()
        };
        assert_eq!(
            super::alert(&state, &silenced, SERVER, f.dm, direct, false),
            None
        );
    }

    #[test]
    fn own_hidden_and_missing_messages_never_alert() {
        let f = Fixture::new();
        let own = f.receive(f.channel, f.own, "me @everyone", &[f.own]);
        let mention = f.receive(f.channel, f.other, "hi", &[f.own]);
        let prefs = Preferences {
            channels: [(format!("{SERVER}/{}", f.channel), Level::All)].into(),
            ..opted_in()
        };
        let state = f.state();
        assert_eq!(alert(&state, &prefs, SERVER, f.channel, own, false), None);
        assert_eq!(
            alert(&state, &prefs, SERVER, f.channel, Id::generate(), false),
            None
        );
        let mut hidden = f.state();
        hidden
            .channels
            .get_mut(&f.channel)
            .unwrap()
            .effective_permissions = Some(
            Permissions::EVERYONE
                .difference(Permissions::VIEW_CHANNEL)
                .bits(),
        );
        assert_eq!(
            alert(&hidden, &prefs, SERVER, f.channel, mention, false),
            None
        );
        let mut unknown = f.state();
        unknown
            .channels
            .get_mut(&f.channel)
            .unwrap()
            .effective_permissions = None;
        assert_eq!(
            alert(&unknown, &prefs, SERVER, f.channel, mention, false),
            None
        );
        let mut offline = f.state();
        offline.connection = ConnectionState::Disconnected;
        assert_eq!(
            alert(&offline, &prefs, SERVER, f.channel, mention, false),
            None
        );
        assert!(alert(&state, &prefs, SERVER, f.channel, mention, false).is_some());
        let muted = Preferences {
            muted_servers: [SERVER.to_owned()].into(),
            ..prefs.clone()
        };
        assert_eq!(
            alert(&state, &muted, SERVER, f.channel, mention, false),
            None
        );
        let dnd = Preferences { dnd: true, ..prefs };
        assert_eq!(alert(&state, &dnd, SERVER, f.channel, mention, false), None);
    }

    #[test]
    fn desktop_body_is_private_unless_previews_are_on() {
        let f = Fixture::new();
        let long = format!("line\none\t{}", "x".repeat(300));
        let mention = f.receive(f.channel, f.other, &long, &[f.own]);
        let direct = f.receive(f.dm, f.other, "secret", &[]);
        let state = f.state();
        let desktop = |prefs: &Preferences, channel, message, focused| {
            alert(&state, prefs, SERVER, channel, message, focused)
                .unwrap()
                .desktop
        };
        let prefs = opted_in();
        assert_eq!(
            desktop(&prefs, f.channel, mention, false).as_deref(),
            Some(CHANNEL_MESSAGE_BODY)
        );
        assert_eq!(
            desktop(&prefs, f.dm, direct, false).as_deref(),
            Some(DIRECT_MESSAGE_BODY)
        );
        // Focus and opting out keep the cue but drop the notification.
        assert_eq!(desktop(&prefs, f.dm, direct, true), None);
        assert_eq!(desktop(&Preferences::default(), f.dm, direct, false), None);
        let previews = Preferences {
            previews: true,
            ..opted_in()
        };
        let preview = desktop(&previews, f.channel, mention, false).unwrap();
        assert_eq!(preview.chars().count(), PREVIEW_CHARS);
        assert!(preview.starts_with("lineonex"));
        assert_eq!(
            desktop(&previews, f.dm, direct, false).as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn alerts_follow_the_current_cache_not_the_event() {
        let f = Fixture::new();
        let message = f.receive(f.channel, f.other, "hi", &[f.own]);
        f.store
            .apply(Msg::MessageUpdated(v1::MessageUpdated {
                message: Some(v1::Message {
                    id: wire(message),
                    channel_id: wire(f.channel),
                    author: Some(user(f.other)),
                    content: "edited".into(),
                    ..Default::default()
                }),
                nonce: None,
            }))
            .unwrap();
        // The edit removed the mention.
        assert_eq!(
            alert(&f.state(), &opted_in(), SERVER, f.channel, message, false),
            None
        );
    }

    #[test]
    fn each_message_alerts_once_and_hidden_ones_are_not_remembered() {
        let f = Fixture::new();
        let mut tracker = Tracker::default();
        let state = f.state();
        let message = Id::generate();
        assert!(tracker.first_sighting(&state, SERVER, f.channel, message));
        assert!(!tracker.first_sighting(&state, SERVER, f.channel, message));
        // The same id on another server is another message.
        assert!(tracker.first_sighting(&state, "example.org:5781", f.channel, message));
        let mut hidden = f.state();
        hidden
            .channels
            .get_mut(&f.channel)
            .unwrap()
            .effective_permissions = Some(Permissions::empty().bits());
        let later = Id::generate();
        assert!(!tracker.first_sighting(&hidden, SERVER, f.channel, later));
        assert!(tracker.first_sighting(&state, SERVER, f.channel, later));
    }

    #[test]
    fn dedupe_remembers_the_512_highest_ids() {
        let f = Fixture::new();
        let state = f.state();
        let mut tracker = Tracker::default();
        let ids: Vec<Id> = (0..=u16::try_from(Tracker::SEEN).unwrap())
            .map(|n| {
                let mut bytes = [0; 16];
                bytes[14..].copy_from_slice(&n.to_be_bytes());
                Id::from(bytes)
            })
            .collect();
        for id in &ids {
            assert!(tracker.first_sighting(&state, SERVER, f.channel, *id));
        }
        assert_eq!(tracker.seen.len(), Tracker::SEEN);
        // The lowest id was evicted; every later one is still refused.
        for id in &ids[1..] {
            assert!(!tracker.first_sighting(&state, SERVER, f.channel, *id));
        }
        assert!(tracker.first_sighting(&state, SERVER, f.channel, ids[0]));
    }

    #[test]
    fn badge_sums_visible_mentions_across_servers() {
        let f = Fixture::new();
        let hidden = Id::generate();
        let mut state = f.state();
        state
            .channels
            .insert(hidden, channel(hidden, Permissions::empty()));
        for (channel, count) in [(f.channel, 2), (f.dm, 1), (hidden, 5)] {
            state.read_states.insert(
                channel,
                v1::ReadState {
                    channel_id: wire(channel),
                    mention_count: count,
                    ..Default::default()
                },
            );
        }
        assert_eq!(mention_count(&state), 3);
        let mut tracker = Tracker::default();
        assert_eq!(tracker.badge("other:5781", &State::default()), None);
        assert_eq!(tracker.badge(SERVER, &state), Some(3));
        let mut second = f.state();
        second.read_states.insert(
            f.channel,
            v1::ReadState {
                channel_id: wire(f.channel),
                mention_count: 4,
                ..Default::default()
            },
        );
        assert_eq!(tracker.badge("other:5781", &second), Some(7));
        state.connection = ConnectionState::Reconnecting {
            attempt: 1,
            delay: Duration::from_secs(1),
        };
        assert_eq!(mention_count(&state), 0);
        assert_eq!(tracker.badge(SERVER, &state), Some(4));
        assert_eq!(tracker.badge("other:5781", &State::default()), None);
    }

    #[test]
    fn mention_badge_follows_live_messages() {
        let f = Fixture::new();
        let _ = f.receive(f.channel, f.other, "hi", &[f.own]);
        let _ = f.receive(f.channel, f.other, "hi @everyone", &[]);
        let _ = f.receive(f.channel, f.other, "plain", &[]);
        assert_eq!(mention_count(&f.state()), 2);
    }

    fn in_voice(state: &mut State, user: Id, channel: Option<Id>) {
        state.voice_states.insert(
            Id::generate(),
            v1::VoiceState {
                user_id: wire(user),
                channel_id: channel.map(wire),
                ..Default::default()
            },
        );
    }

    #[test]
    fn voice_cues_diff_the_call_between_events() {
        let f = Fixture::new();
        let (call, elsewhere) = (Id::generate(), Id::generate());
        let prefs = Preferences::default();
        let mut tracker = Tracker::default();
        let mut state = f.state();
        in_voice(&mut state, f.own, Some(call));
        let changed = ClientEvent::VoiceChanged;
        // The first event after joining has nothing to compare with.
        assert_eq!(
            tracker.voice_cue(SERVER, Some(call), &state, &changed, &prefs),
            None
        );
        in_voice(&mut state, f.other, Some(elsewhere));
        assert_eq!(
            tracker.voice_cue(SERVER, Some(call), &state, &changed, &prefs),
            None
        );
        let joined = state.clone();
        in_voice(&mut state, f.other, Some(call));
        assert_eq!(
            tracker.voice_cue(SERVER, Some(call), &state, &changed, &prefs),
            Some(Cue::Join)
        );
        assert_eq!(
            tracker.voice_cue(SERVER, Some(call), &joined, &changed, &prefs),
            Some(Cue::Leave)
        );
        // Moving to another channel starts the comparison again.
        assert_eq!(
            tracker.voice_cue(SERVER, Some(elsewhere), &state, &changed, &prefs),
            None
        );
        // Leaving the call forgets it.
        assert_eq!(
            tracker.voice_cue(SERVER, None, &state, &changed, &prefs),
            None
        );
        assert_eq!(
            tracker.voice_cue(SERVER, Some(call), &joined, &changed, &prefs),
            None
        );
        // Only VoiceChanged compares members.
        assert_eq!(
            tracker.voice_cue(SERVER, Some(call), &state, &ClientEvent::Resynced, &prefs),
            Some(Cue::Restored)
        );
    }

    #[test]
    fn a_swap_of_members_plays_join() {
        let f = Fixture::new();
        let call = Id::generate();
        let prefs = Preferences::default();
        let mut tracker = Tracker::default();
        let mut before = f.state();
        in_voice(&mut before, f.own, Some(call));
        let mut after = before.clone();
        in_voice(&mut before, f.other, Some(call));
        in_voice(&mut after, Id::generate(), Some(call));
        tracker.voice_cue(
            SERVER,
            Some(call),
            &before,
            &ClientEvent::VoiceChanged,
            &prefs,
        );
        assert_eq!(
            tracker.voice_cue(
                SERVER,
                Some(call),
                &after,
                &ClientEvent::VoiceChanged,
                &prefs
            ),
            Some(Cue::Join)
        );
    }

    #[test]
    fn connection_cues_play_once_per_outage_and_respect_sounds() {
        let f = Fixture::new();
        let call = Some(Id::generate());
        let state = f.state();
        let mut tracker = Tracker::default();
        let reconnecting = |attempt| {
            ClientEvent::Connection(ConnectionState::Reconnecting {
                attempt,
                delay: Duration::from_secs(1),
            })
        };
        let prefs = Preferences::default();
        assert_eq!(
            tracker.voice_cue(SERVER, call, &state, &reconnecting(1), &prefs),
            Some(Cue::Lost)
        );
        assert_eq!(
            tracker.voice_cue(SERVER, call, &state, &reconnecting(2), &prefs),
            None
        );
        assert_eq!(
            tracker.voice_cue(SERVER, call, &state, &ClientEvent::Resynced, &prefs),
            Some(Cue::Restored)
        );
        assert_eq!(
            tracker.voice_cue(SERVER, None, &state, &ClientEvent::Resynced, &prefs),
            None
        );
        for silent in [
            Preferences {
                dnd: true,
                ..Preferences::default()
            },
            Preferences {
                muted_servers: [SERVER.to_owned()].into(),
                ..Preferences::default()
            },
        ] {
            assert_eq!(
                tracker.voice_cue(SERVER, call, &state, &reconnecting(1), &silent),
                None
            );
            assert_eq!(
                tracker.voice_cue(SERVER, call, &state, &ClientEvent::Resynced, &silent),
                None
            );
        }
    }

    #[test]
    fn silenced_events_still_update_the_call() {
        let f = Fixture::new();
        let call = Id::generate();
        let mut tracker = Tracker::default();
        let mut state = f.state();
        in_voice(&mut state, f.own, Some(call));
        let dnd = Preferences {
            dnd: true,
            ..Preferences::default()
        };
        tracker.voice_cue(SERVER, Some(call), &state, &ClientEvent::VoiceChanged, &dnd);
        in_voice(&mut state, f.other, Some(call));
        tracker.voice_cue(SERVER, Some(call), &state, &ClientEvent::VoiceChanged, &dnd);
        // Turning sounds back on compares against the silenced event, not
        // against the call before it.
        assert_eq!(
            tracker.voice_cue(
                SERVER,
                Some(call),
                &state,
                &ClientEvent::VoiceChanged,
                &Preferences::default()
            ),
            None
        );
    }

    #[test]
    fn presence_keeps_activity_and_resync_reasserts_dnd() {
        let f = Fixture::new();
        let state = f.state();
        let dnd = presence(&state, true);
        assert_eq!(dnd.status, v1::PresenceStatus::Dnd as i32);
        assert_eq!(dnd.activity, "Reviewing");
        assert_eq!(
            presence(&state, false).status,
            v1::PresenceStatus::Online as i32
        );
        assert_eq!(presence(&State::default(), true).activity, "");
        let on = Preferences {
            dnd: true,
            ..Preferences::default()
        };
        assert_eq!(
            presence_after(&ClientEvent::Resynced, &on),
            Some(v1::SetPresence {
                status: v1::PresenceStatus::Dnd as i32,
                activity: String::new(),
            })
        );
        assert_eq!(presence_after(&ClientEvent::VoiceChanged, &on), None);
        assert_eq!(
            presence_after(&ClientEvent::Resynced, &Preferences::default()),
            None
        );
    }
}
