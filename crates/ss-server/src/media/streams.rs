//! Permission-checked opaque video tracks and bounded per-viewer group relay.
//!
//! A publisher may announce a second, lower rendition. Each subscriber that
//! declared it decodes both is forwarded one of them, chosen per subscriber
//! in [`adapt`] and switched only when a new group (a keyframe) begins.
mod adapt;
mod relay;
use crate::{
    handlers::Error,
    perms::PermissionService,
    registry::ConnectionRegistry,
    voice::{Outcome, rooms::Rooms},
};
use adapt::{Link, Selection};
use parking_lot::RwLock;
use ss_core::{Id, Permissions};
use ss_proto::{v1, validate::id_to_bytes};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use v1::{client_envelope::Msg, server_envelope::Msg as Event};

/// Groups retained per rendition, for the publisher and for each subscriber.
const GROUP_BUDGET: usize = 2;
const MAX_SUBSCRIPTIONS: usize = 16;
const KEYFRAME_INTERVAL: Duration = Duration::from_millis(500);
/// A track carries the full rendition and at most one lower one.
const MAX_RENDITIONS: usize = 2;
/// Largest per-rendition codec description.
const MAX_RENDITION_CONFIG: usize = 4096;

/// Video relay counters, separate from the allocation-free audio path.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    /// Published screen/video tracks.
    pub active: usize,
    /// Bytes written to downstream streams, including their prefixes.
    pub bytes_forwarded: u64,
    /// Of `bytes_forwarded`, bytes per rendition (full, low).
    pub bytes_by_rendition: [u64; MAX_RENDITIONS],
    /// Downstream groups reset due to cancellation, overflow, or source failure.
    pub resets: u64,
    /// Invalid, unauthorized, or resource-limited incoming groups.
    pub rejected: u64,
    /// Subscribers moved to a lower rendition.
    pub switches_down: u64,
    /// Subscribers moved back to a higher rendition.
    pub switches_up: u64,
}
#[derive(Default)]
struct Counters {
    bytes: AtomicU64,
    rendition_bytes: [AtomicU64; MAX_RENDITIONS],
    resets: AtomicU64,
    rejected: AtomicU64,
    down: AtomicU64,
    up: AtomicU64,
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
/// One retained group: its cancel switch and whether its transfer finished.
#[derive(Clone)]
struct Group {
    cancel: Cancel,
    done: Arc<AtomicBool>,
}
impl Group {
    fn new() -> Self {
        Self {
            cancel: Cancel::new(),
            done: Arc::default(),
        }
    }
    fn live(&self) -> bool {
        !self.done.load(Ordering::Relaxed)
    }
}
/// Per-track, per-rendition counters reported to the publisher each second.
#[derive(Default)]
struct RenditionCounters {
    bytes: [AtomicU64; MAX_RENDITIONS],
    resets: [AtomicU32; MAX_RENDITIONS],
}
struct Subscriber {
    user: Id,
    connection: quinn::Connection,
    height: u32,
    fps: u32,
    link: Arc<Link>,
    /// Rendition choice; always rendition 0 for a viewer that did not opt in.
    select: Selection,
    groups: [VecDeque<Group>; MAX_RENDITIONS],
}
impl Subscriber {
    fn cancel(self) {
        for groups in self.groups {
            cancel_all(groups);
        }
    }
}
struct Track {
    wire: v1::Track,
    owner: Id,
    channel: Id,
    user: Id,
    viewers: HashMap<Id, Subscriber>,
    /// Incoming groups per rendition. A finished group is never cancelled
    /// from here, so a healthy viewer's tail is not reset at a boundary.
    groups: [VecDeque<Group>; MAX_RENDITIONS],
    last_keyframe: [Option<Instant>; MAX_RENDITIONS],
    counters: Arc<RenditionCounters>,
    /// Low-rendition subscriber count last reported to the publisher.
    reported_low: u32,
}
impl Track {
    fn renditions(&self) -> usize {
        self.wire.renditions.len().max(1)
    }
    fn low_subscribers(&self) -> u32 {
        let count = self
            .viewers
            .values()
            .filter(|s| s.select.receives_low())
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }
}
pub(crate) struct State {
    tracks: HashMap<u32, Track>,
    links: HashMap<Id, Arc<Link>>,
    registry: Arc<ConnectionRegistry>,
    counters: Arc<Counters>,
}
impl State {
    pub(crate) fn new(registry: Arc<ConnectionRegistry>) -> Self {
        Self {
            tracks: HashMap::new(),
            links: HashMap::new(),
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
                sub.cancel();
                hint(&self.registry, track);
                report_low_change(&self.registry, track);
            }
        }
        self.links.remove(&conn);
        events
    }
    fn remove(&mut self, id: u32) -> Option<Event> {
        let track = self.tracks.remove(&id)?;
        for groups in track.groups {
            cancel_all(groups);
        }
        for (_, sub) in track.viewers {
            sub.cancel();
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
                    sub.cancel();
                }
            }
            if changed {
                hint(&self.registry, track);
                report_low_change(&self.registry, track);
            }
        }
        events
    }
    fn link(&mut self, conn: Id) -> Arc<Link> {
        Arc::clone(self.links.entry(conn).or_default())
    }
}
fn cancel_all(groups: VecDeque<Group>) {
    for group in groups {
        group.cancel.stop();
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
        renditions: t.renditions.clone(),
    })
}
fn hint(registry: &ConnectionRegistry, track: &Track) {
    // The hint picks the full rendition's preset. Use the viewers' highest
    // requested limits; any unspecified preference leaves that dimension
    // uncapped (zero). The relay serves lower preferences from the low
    // rendition when there is one.
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

/// Send `SubscriberStats` at once when the low rendition gains its first
/// subscriber or loses its last, so the publisher starts or stops its low
/// encoder without waiting for the next report.
fn report_low_change(registry: &ConnectionRegistry, track: &mut Track) {
    if track.renditions() < 2 {
        return;
    }
    let low = track.low_subscribers();
    if (low == 0) != (track.reported_low == 0) {
        registry.enqueue(track.owner, stats_envelope(track, false));
    }
}

/// Per-rendition report for the publisher. `drain` resets the interval counters.
fn stats_envelope(track: &mut Track, drain: bool) -> v1::ServerEnvelope {
    let renditions = (0..track.renditions())
        .map(|index| {
            let on = track
                .viewers
                .values()
                .filter(|s| usize::from(s.select.rendition()) == index);
            let (mut subscribers, mut congested, mut backlog) = (0_u32, 0_u32, 0_u32);
            for sub in on {
                subscribers += 1;
                if sub.link.congested() {
                    congested += 1;
                }
                backlog = backlog.max(sub.link.backlog_ms());
            }
            if index == 1 {
                // A viewer probing the full rendition still receives the low one.
                subscribers = track.low_subscribers();
            }
            let (bytes, resets) = if drain {
                (
                    track.counters.bytes[index].swap(0, Ordering::Relaxed),
                    track.counters.resets[index].swap(0, Ordering::Relaxed),
                )
            } else {
                (0, 0)
            };
            v1::RenditionStats {
                index: u32::try_from(index).unwrap_or(0),
                subscribers,
                congested,
                resets,
                max_backlog_ms: backlog,
                bytes,
            }
        })
        .collect();
    track.reported_low = track.low_subscribers();
    v1::ServerEnvelope {
        request_id: 0,
        msg: Some(Event::SubscriberStats(v1::SubscriberStats {
            track_id: track.wire.track_id,
            renditions,
        })),
    }
}

/// Owns video policy and receives uni streams after authenticated Hello.
pub struct Streams {
    rooms: Arc<RwLock<Rooms>>,
    perms: Arc<PermissionService>,
    counters: Arc<Counters>,
}
impl Streams {
    pub(crate) fn new(rooms: Arc<RwLock<Rooms>>, perms: Arc<PermissionService>) -> Self {
        let counters = Arc::clone(&rooms.read().video.counters);
        Self {
            rooms,
            perms,
            counters,
        }
    }
    /// Current publication count and cumulative forwarding/reset totals.
    pub fn stats(&self) -> Stats {
        Stats {
            active: self.rooms.read().video.tracks.len(),
            bytes_forwarded: self.counters.bytes.load(Ordering::Relaxed),
            bytes_by_rendition: [
                self.counters.rendition_bytes[0].load(Ordering::Relaxed),
                self.counters.rendition_bytes[1].load(Ordering::Relaxed),
            ],
            resets: self.counters.resets.load(Ordering::Relaxed),
            rejected: self.counters.rejected.load(Ordering::Relaxed),
            switches_down: self.counters.down.load(Ordering::Relaxed),
            switches_up: self.counters.up.load(Ordering::Relaxed),
        }
    }
    pub(crate) fn dispatch(
        &self,
        conn: Id,
        user: Id,
        command: &Msg,
    ) -> Option<Result<Outcome, Error>> {
        match command {
            Msg::StartStream(_)
            | Msg::StopStream(_)
            | Msg::SubscribeTrack(_)
            | Msg::UnsubscribeTrack(_)
            | Msg::SetPreferredQuality(_)
            | Msg::RequestKeyframe(_) => Some(self.command(conn, user, command)),
            _ => None,
        }
    }
    fn command(&self, conn: Id, user: Id, command: &Msg) -> Result<Outcome, Error> {
        let mut rooms = self.rooms.write();
        let mut outcome = Outcome {
            ack: v1::ack::Result::Empty(v1::Empty {}),
            broadcast: Vec::new(),
            direct: Vec::new(),
        };
        if matches!(command, Msg::StopStream(_)) {
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
        if let Msg::StartStream(start) = command {
            return start_track(&mut rooms, &self.perms, conn, user, channel, start);
        }
        let id = match command {
            Msg::SubscribeTrack(c) => c.track_id,
            Msg::UnsubscribeTrack(c) => c.track_id,
            Msg::SetPreferredQuality(c) => c.track_id,
            Msg::RequestKeyframe(c) => c.track_id,
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
        let link = rooms.video.link(conn);
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
            Msg::SubscribeTrack(subscribe) => {
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
                        link,
                        select: Selection::new(subscribe.renditions, Instant::now()),
                        groups: Default::default(),
                    });
                    hint(&registry, track);
                }
            }
            Msg::UnsubscribeTrack(_) => {
                if let Some(sub) = track.viewers.remove(&conn) {
                    sub.cancel();
                    hint(&registry, track);
                    report_low_change(&registry, track);
                }
            }
            Msg::SetPreferredQuality(quality) => prefer(&registry, track, conn, quality)?,
            Msg::RequestKeyframe(_) => {
                let rendition = track
                    .viewers
                    .get(&conn)
                    .ok_or(Error::Forbidden("subscribe before requesting keyframes"))?
                    .select
                    .rendition();
                request_keyframe(&registry, track, rendition);
            }
            _ => {}
        }
        Ok(outcome)
    }
}

/// Store a viewer's preference and cap its rendition by it.
fn prefer(
    registry: &ConnectionRegistry,
    track: &mut Track,
    conn: Id,
    quality: &v1::SetPreferredQuality,
) -> Result<(), Error> {
    if quality.max_height > 4320 || quality.max_fps > 240 {
        return Err(Error::Invalid("video preference exceeds limit"));
    }
    let cap = adapt::cap(&track.wire.renditions, quality.max_height, quality.max_fps);
    let sub = track
        .viewers
        .get_mut(&conn)
        .ok_or(Error::Forbidden("subscribe before setting quality"))?;
    sub.height = quality.max_height;
    sub.fps = quality.max_fps;
    let effect = sub.select.set_cap(cap, Instant::now());
    if effect == adapt::Cap::ProbeEnded {
        // The probe's full groups are over the cap; the low rendition kept flowing.
        cancel_all(std::mem::take(&mut sub.groups[0]));
    }
    hint(registry, track);
    if effect == adapt::Cap::Down {
        // Start the capped viewer on the low rendition's next group.
        report_low_change(registry, track);
        request_keyframe(registry, track, 1);
    }
    Ok(())
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
    ) || !valid_codec(&codec)
    {
        return Err(Error::Invalid("invalid video codec or kind"));
    }
    if !valid_renditions(&start.renditions) {
        return Err(Error::Invalid("invalid video renditions"));
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
        renditions: start.renditions.clone(),
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
            groups: Default::default(),
            last_keyframe: [None; MAX_RENDITIONS],
            counters: Arc::default(),
            reported_low: 0,
        },
    );
    Ok(outcome)
}

fn valid_codec(codec: &v1::Codec) -> bool {
    !codec.name.is_empty()
        && codec.name.len() <= 128
        && codec.description.len() <= 65536
        && codec.sample_rate == 0
        && codec.channels == 0
}

/// Empty, or the full rendition then at most one lower one, numbered in order.
fn valid_renditions(renditions: &[v1::Rendition]) -> bool {
    renditions.len() <= MAX_RENDITIONS
        && renditions.iter().enumerate().all(|(i, r)| {
            usize::try_from(r.index).is_ok_and(|index| index == i)
                && (1..=7680).contains(&r.width)
                && (1..=4320).contains(&r.height)
                && (1..=240).contains(&r.fps)
                && r.codec.as_ref().is_none_or(|codec| {
                    // An H.264 or HEVC configuration is well under 1 KiB; the
                    // bound keeps a track in a Welcome small.
                    valid_codec(codec) && codec.description.len() <= MAX_RENDITION_CONFIG
                })
        })
        && renditions
            .windows(2)
            .all(|pair| pair[1].height <= pair[0].height && pair[1].fps <= pair[0].fps)
}

/// Relay a keyframe request for one rendition, at most once per 500 ms each.
fn request_keyframe(registry: &ConnectionRegistry, track: &mut Track, rendition: u8) {
    let index = usize::from(rendition).min(MAX_RENDITIONS - 1);
    let now = Instant::now();
    if track.last_keyframe[index].is_none_or(|last| now.duration_since(last) >= KEYFRAME_INTERVAL) {
        track.last_keyframe[index] = Some(now);
        registry.enqueue(
            track.owner,
            v1::ServerEnvelope {
                request_id: 0,
                msg: Some(Event::RequestKeyframe(v1::RequestKeyframe {
                    track_id: track.wire.track_id,
                    rendition: u32::from(rendition),
                })),
            },
        );
    }
}
