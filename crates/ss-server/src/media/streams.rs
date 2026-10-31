//! Permission-checked opaque video tracks and bounded per-viewer group relay.
mod relay;
use crate::{
    handlers::Error,
    perms::PermissionService,
    registry::ConnectionRegistry,
    voice::{Outcome, rooms::Rooms},
};
use parking_lot::Mutex;
use ss_core::{Id, Permissions};
use ss_proto::{v1, validate::id_to_bytes};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use v1::{client_envelope::Msg as Command, server_envelope::Msg as Event};

const GROUP_BUDGET: usize = 2;
const MAX_SUBSCRIPTIONS: usize = 16;
const KEYFRAME_INTERVAL: Duration = Duration::from_millis(500);

/// Video relay counters, separate from the allocation-free audio path.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    /// Published screen/video tracks.
    pub active: usize,
    /// Bytes written to downstream streams, including their prefixes.
    pub bytes_forwarded: u64,
    /// Downstream groups reset due to cancellation, overflow, or source failure.
    pub resets: u64,
    /// Invalid, unauthorized, or resource-limited incoming groups.
    pub rejected: u64,
}
#[derive(Default)]
struct Counters {
    bytes: AtomicU64,
    resets: AtomicU64,
    rejected: AtomicU64,
}
#[derive(Clone)]
struct Cancel(tokio::sync::watch::Sender<bool>);
impl Cancel {
    fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
    fn stop(&self) {
        self.0.send_replace(true);
    }
    async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        if !*rx.borrow_and_update() {
            let _ = rx.changed().await;
        }
    }
}
struct Subscriber {
    user: Id,
    connection: quinn::Connection,
    height: u32,
    fps: u32,
    groups: VecDeque<Cancel>,
}
struct Track {
    wire: v1::Track,
    owner: Id,
    channel: Id,
    user: Id,
    viewers: HashMap<Id, Subscriber>,
    groups: VecDeque<Cancel>,
    last_keyframe: Option<Instant>,
}
pub(crate) struct State {
    tracks: HashMap<u32, Track>,
    registry: Arc<ConnectionRegistry>,
    counters: Arc<Counters>,
}
impl State {
    pub(crate) fn new(registry: Arc<ConnectionRegistry>) -> Self {
        Self {
            tracks: HashMap::new(),
            registry,
            counters: Arc::default(),
        }
    }
    pub(crate) fn snapshot(&self, visible: impl Fn(Id) -> bool) -> Vec<v1::Track> {
        self.tracks
            .values()
            .filter(|t| visible(t.channel))
            .map(|t| t.wire.clone())
            .collect()
    }
    pub(crate) fn channel_events(&self, channel: Id) -> Vec<Event> {
        self.tracks
            .values()
            .filter(|t| t.channel == channel)
            .map(|t| added(&t.wire))
            .collect()
    }
    pub(crate) fn depart(&mut self, conn: Id) -> Vec<Event> {
        let owned: Vec<_> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.owner == conn)
            .map(|(id, _)| *id)
            .collect();
        let events = owned.into_iter().filter_map(|id| self.remove(id)).collect();
        for track in self.tracks.values_mut() {
            if let Some(sub) = track.viewers.remove(&conn) {
                cancel_all(sub.groups);
                hint(&self.registry, track);
            }
        }
        events
    }
    fn remove(&mut self, id: u32) -> Option<Event> {
        let track = self.tracks.remove(&id)?;
        cancel_all(track.groups);
        for (_, sub) in track.viewers {
            cancel_all(sub.groups);
        }
        Some(Event::TrackRemoved(v1::TrackRemoved { track_id: id }))
    }
    pub(crate) fn refresh(&mut self, perms: &PermissionService) -> Vec<Event> {
        let revoked: Vec<_> = self
            .tracks
            .iter()
            .filter(|(_, t)| {
                !allowed(
                    perms,
                    t.user,
                    t.channel,
                    Permissions::VIEW_CHANNEL | Permissions::STREAM,
                )
            })
            .map(|(id, _)| *id)
            .collect();
        let events = revoked
            .into_iter()
            .filter_map(|id| self.remove(id))
            .collect();
        for track in self.tracks.values_mut() {
            let removed: Vec<_> = track
                .viewers
                .iter()
                .filter(|(_, s)| !allowed(perms, s.user, track.channel, Permissions::VIEW_CHANNEL))
                .map(|(id, _)| *id)
                .collect();
            let changed = !removed.is_empty();
            for conn in removed {
                if let Some(sub) = track.viewers.remove(&conn) {
                    cancel_all(sub.groups);
                }
            }
            if changed {
                hint(&self.registry, track);
            }
        }
        events
    }
}
fn cancel_all(groups: VecDeque<Cancel>) {
    for group in groups {
        group.stop();
    }
}
fn allowed(perms: &PermissionService, user: Id, channel: Id, permission: Permissions) -> bool {
    perms.check(user, Some(channel), permission).is_ok()
}
fn added(t: &v1::Track) -> Event {
    Event::TrackAdded(v1::TrackAdded {
        track_id: t.track_id,
        user_id: t.user_id.clone(),
        conn_id: t.conn_id.clone(),
        kind: t.kind,
        codec: t.codec.clone(),
        channel_id: t.channel_id.clone(),
    })
}
fn hint(registry: &ConnectionRegistry, track: &Track) {
    // One encoded representation serves all viewers. Use their highest requested
    // limits; any unspecified preference leaves that dimension uncapped (zero).
    let height = if track.viewers.values().any(|s| s.height == 0) {
        0
    } else {
        track.viewers.values().map(|s| s.height).max().unwrap_or(0)
    };
    let fps = if track.viewers.values().any(|s| s.fps == 0) {
        0
    } else {
        track.viewers.values().map(|s| s.fps).max().unwrap_or(0)
    };
    registry.enqueue(
        track.owner,
        v1::ServerEnvelope {
            request_id: 0,
            msg: Some(Event::SubscriberHint(v1::SubscriberHint {
                track_id: track.wire.track_id,
                max_height: height,
                max_fps: fps,
                viewer_count: u32::try_from(track.viewers.len()).unwrap_or(u32::MAX),
            })),
        },
    );
}

/// Owns video policy and receives uni streams after authenticated Hello.
pub struct Streams {
    rooms: Arc<Mutex<Rooms>>,
    perms: Arc<PermissionService>,
    counters: Arc<Counters>,
}
impl Streams {
    pub(crate) fn new(rooms: Arc<Mutex<Rooms>>, perms: Arc<PermissionService>) -> Self {
        let counters = Arc::clone(&rooms.lock().video.counters);
        Self {
            rooms,
            perms,
            counters,
        }
    }
    /// Current publication count and cumulative forwarding/reset totals.
    pub fn stats(&self) -> Stats {
        Stats {
            active: self.rooms.lock().video.tracks.len(),
            bytes_forwarded: self.counters.bytes.load(Ordering::Relaxed),
            resets: self.counters.resets.load(Ordering::Relaxed),
            rejected: self.counters.rejected.load(Ordering::Relaxed),
        }
    }
    pub(crate) fn dispatch(
        &self,
        conn: Id,
        user: Id,
        command: &Command,
    ) -> Option<Result<Outcome, Error>> {
        if !matches!(
            command,
            Command::StartStream(_)
                | Command::StopStream(_)
                | Command::SubscribeTrack(_)
                | Command::UnsubscribeTrack(_)
                | Command::SetPreferredQuality(_)
                | Command::RequestKeyframe(_)
        ) {
            return None;
        }
        Some(self.command(conn, user, command))
    }
    fn command(&self, conn: Id, user: Id, command: &Command) -> Result<Outcome, Error> {
        let mut rooms = self.rooms.lock();
        let mut outcome = Outcome {
            ack: v1::ack::Result::Empty(v1::Empty {}),
            broadcast: Vec::new(),
            direct: Vec::new(),
        };
        if matches!(command, Command::StopStream(_)) {
            let id = rooms
                .video
                .tracks
                .iter()
                .find(|(_, t)| t.owner == conn)
                .map(|(id, _)| *id);
            if let Some(id) = id {
                outcome.broadcast.extend(rooms.video.remove(id));
            }
            return Ok(outcome);
        }
        let channel = rooms
            .seat(conn)
            .ok_or(Error::Invalid("join voice before using video"))?
            .channel;
        if !allowed(&self.perms, user, channel, Permissions::VIEW_CHANNEL) {
            return Err(Error::Forbidden("missing view channel"));
        }
        if let Command::StartStream(start) = command {
            return start_track(&mut rooms, &self.perms, conn, user, channel, start);
        }
        let id = match command {
            Command::SubscribeTrack(c) => c.track_id,
            Command::UnsubscribeTrack(c) => c.track_id,
            Command::SetPreferredQuality(c) => c.track_id,
            Command::RequestKeyframe(c) => c.track_id,
            _ => return Err(Error::Invalid("unsupported video command")),
        };
        let subscribed = rooms
            .video
            .tracks
            .values()
            .filter(|t| t.viewers.contains_key(&conn))
            .count();
        let connection = rooms
            .send_target(conn)
            .ok_or(Error::Invalid("unknown connection"))?
            .0
            .clone();
        let registry = Arc::clone(&rooms.video.registry);
        let track = rooms
            .video
            .tracks
            .get_mut(&id)
            .ok_or(Error::NotFound("unknown video track"))?;
        if track.channel != channel || track.owner == conn {
            return Err(Error::Forbidden("video belongs to another channel or self"));
        }
        match command {
            Command::SubscribeTrack(_) => {
                if let std::collections::hash_map::Entry::Vacant(entry) = track.viewers.entry(conn)
                {
                    if subscribed >= MAX_SUBSCRIPTIONS {
                        return Err(Error::Invalid("too many video subscriptions"));
                    }
                    entry.insert(Subscriber {
                        user,
                        connection,
                        height: 0,
                        fps: 0,
                        groups: VecDeque::new(),
                    });
                    hint(&registry, track);
                }
            }
            Command::UnsubscribeTrack(_) => {
                if let Some(sub) = track.viewers.remove(&conn) {
                    cancel_all(sub.groups);
                    hint(&registry, track);
                }
            }
            Command::SetPreferredQuality(quality) => {
                if quality.max_height > 4320 || quality.max_fps > 240 {
                    return Err(Error::Invalid("video preference exceeds limit"));
                }
                let sub = track
                    .viewers
                    .get_mut(&conn)
                    .ok_or(Error::Forbidden("subscribe before setting quality"))?;
                sub.height = quality.max_height;
                sub.fps = quality.max_fps;
                hint(&registry, track);
            }
            Command::RequestKeyframe(_) => request_keyframe(&registry, track, conn)?,
            _ => {}
        }
        Ok(outcome)
    }
}

fn start_track(
    rooms: &mut Rooms,
    perms: &PermissionService,
    conn: Id,
    user: Id,
    channel: Id,
    start: &v1::StartStream,
) -> Result<Outcome, Error> {
    let mut outcome = Outcome {
        ack: v1::ack::Result::Empty(v1::Empty {}),
        broadcast: Vec::new(),
        direct: Vec::new(),
    };
    if !allowed(perms, user, channel, Permissions::STREAM) {
        return Err(Error::Forbidden("missing stream"));
    }
    let codec = start
        .codec
        .clone()
        .ok_or(Error::Invalid("missing video codec"))?;
    if !matches!(
        v1::TrackKind::try_from(start.kind),
        Ok(v1::TrackKind::Screen | v1::TrackKind::Video)
    ) || codec.name.is_empty()
        || codec.name.len() > 128
        || codec.description.len() > 65536
        || codec.sample_rate != 0
        || codec.channels != 0
    {
        return Err(Error::Invalid("invalid video codec or kind"));
    }
    if rooms.video.tracks.values().any(|t| t.owner == conn) {
        return Err(Error::Invalid("one video track per connection"));
    }
    let id = rooms
        .allocate_track()
        .ok_or(Error::Invalid("track id space exhausted"))?;
    let wire = v1::Track {
        track_id: id,
        user_id: id_to_bytes(user),
        conn_id: id_to_bytes(conn),
        kind: start.kind,
        codec: Some(codec),
        channel_id: id_to_bytes(channel),
    };
    outcome.broadcast.push(added(&wire));
    outcome.ack = v1::ack::Result::TrackId(id);
    rooms.video.tracks.insert(
        id,
        Track {
            wire,
            owner: conn,
            user,
            channel,
            viewers: HashMap::new(),
            groups: VecDeque::new(),
            last_keyframe: None,
        },
    );
    Ok(outcome)
}

fn request_keyframe(
    registry: &ConnectionRegistry,
    track: &mut Track,
    conn: Id,
) -> Result<(), Error> {
    if !track.viewers.contains_key(&conn) {
        return Err(Error::Forbidden("subscribe before requesting keyframes"));
    }
    let now = Instant::now();
    if track
        .last_keyframe
        .is_none_or(|last| now.duration_since(last) >= KEYFRAME_INTERVAL)
    {
        track.last_keyframe = Some(now);
        registry.enqueue(
            track.owner,
            v1::ServerEnvelope {
                request_id: 0,
                msg: Some(Event::RequestKeyframe(v1::RequestKeyframe {
                    track_id: track.wire.track_id,
                })),
            },
        );
    }
    Ok(())
}
