//! Authenticated connections and the presence derived from them.
//!
//! A user is online while at least one connection is registered. The first
//! connection emits `PresenceUpdated`; the last disconnect emits offline.
//! Explicit idle and do-not-disturb survive a reconnect until the process
//! restarts.
//!
//! Every connection has two control queues. Its own session loop fills the
//! first, of [`OUTBOUND_CAPACITY`] frames, with replies and bus events and
//! waits for room, so a reader that falls behind lags on the bus and is
//! closed. The second, a [`DirectQueue`], carries frames other tasks address
//! to it with [`ConnectionRegistry::enqueue`]. Past [`OUTBOUND_CAPACITY`] it
//! keeps accepting frames for [`ConnectionRegistry::slow_after`] (a link that
//! is briefly slow is not punished for what other users trigger), then the
//! next frame closes the connection; [`DIRECT_LIMIT`] bounds it outright.
//! Per-track `SubscriberHint`, `SubscriberStats` and per-rendition
//! `RequestKeyframe` frames replace a pending one for the same track instead
//! of taking another slot. A frame is never
//! dropped while the connection stays open: the client resyncs from
//! `Welcome` instead.
//!
//! Each connection also remembers which voice connections it was told are in
//! a channel. A `VoiceStateUpdated` without a channel (a leave) names no
//! channel to check visibility against, so it is only sent to connections
//! that saw the matching arrival. See [`ConnectionRegistry::admit_voice`].

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use ss_core::Id;
use ss_proto::{v1, validate::id_to_bytes};
use tokio::sync::mpsc;

/// QUIC close code for [`ConnectionRegistry::shutdown`]. Clients reconnect
/// with backoff.
pub const CLOSE_SHUTDOWN: u32 = 1;
/// QUIC close code when a control consumer stays wedged.
pub const CLOSE_SLOW: u32 = 2;
/// QUIC close code after a `Kicked` frame.
pub const CLOSE_KICKED: u32 = 3;
/// QUIC close code after a `Banned` frame.
pub const CLOSE_BANNED: u32 = 4;
/// Repeated media bitrate violations.
pub const CLOSE_RATE_LIMITED: u32 = 5;
/// How long one stalled write to the control stream is tolerated.
pub const SLOW_CONSUMER_AFTER: Duration = Duration::from_secs(10);
/// Size of the session's own control queue, and how full a [`DirectQueue`]
/// may get before its grace period starts.
pub const OUTBOUND_CAPACITY: usize = 32;
/// Frames a [`DirectQueue`] holds at most, grace or not. Reaching it closes
/// the connection.
pub const DIRECT_LIMIT: usize = 1024;

const SLOW_REASON: &[u8] = b"control consumer too slow";
const SHUTDOWN_REASON: &[u8] = b"server shutting down";
const SLOW_NOTICE: &str = "control consumer too slow";
const SHUTDOWN_NOTICE: &str = "server shutting down";

/// Product string from `Hello`, kept for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    /// `Hello.client_name`.
    pub name: String,
    /// `Hello.client_version`.
    pub version: String,
    /// `Hello.os`.
    pub os: String,
}

/// What [`ConnectionRegistry::register`] needs from the accept loop.
pub(crate) struct Registration {
    /// This QUIC connection.
    pub conn_id: Id,
    /// Account that passed `Hello`.
    pub user_id: Id,
    /// Used to close a slow or shutting-down peer.
    pub connection: quinn::Connection,
    /// The session loop's own bounded queue, kept for diagnostics.
    pub outbound: mpsc::Sender<v1::ServerEnvelope>,
    /// Frames other tasks address to this connection. The session writer
    /// drains it.
    pub direct: Arc<DirectQueue>,
    /// One-slot signal: send `ServerNotice`, then close.
    pub kicks: mpsc::Sender<Kick>,
    /// Client product strings.
    pub client: ClientInfo,
}

/// Notice plus the QUIC close that follows it.
pub(crate) struct Kick {
    /// Frame the writer sends before closing.
    pub notice: v1::ServerEnvelope,
    /// Application close code.
    pub code: u32,
    /// Close reason, matching the notice text.
    pub reason: &'static [u8],
}

#[derive(Default)]
struct PresenceBook {
    users: HashMap<Id, UserPresence>,
    conn_user: HashMap<Id, Id>,
}

struct UserPresence {
    conns: Vec<Id>,
    status: i32,
    activity: String,
}

impl PresenceBook {
    fn connect(&mut self, user: Id, conn: Id) -> Option<v1::server_envelope::Msg> {
        let entry = self.users.entry(user).or_insert_with(|| UserPresence {
            conns: Vec::new(),
            status: i32::from(v1::PresenceStatus::Online),
            activity: String::new(),
        });
        let first = entry.conns.is_empty();
        entry.conns.push(conn);
        self.conn_user.insert(conn, user);
        let status = entry.status;
        let activity = entry.activity.clone();
        first.then(|| presence_event(user, status, activity))
    }

    fn disconnect(&mut self, conn: Id) -> Option<v1::server_envelope::Msg> {
        let user = self.conn_user.remove(&conn)?;
        let entry = self.users.get_mut(&user)?;
        if let Some(pos) = entry.conns.iter().position(|id| *id == conn) {
            let _ = entry.conns.remove(pos);
        }
        entry
            .conns
            .is_empty()
            .then(|| presence_event(user, i32::from(v1::PresenceStatus::Offline), String::new()))
    }

    fn set(
        &mut self,
        user: Id,
        status: i32,
        activity: String,
    ) -> Result<Option<v1::server_envelope::Msg>, &'static str> {
        let Some(entry) = self.users.get_mut(&user) else {
            return Err("not connected");
        };
        if entry.conns.is_empty() {
            return Err("not connected");
        }
        let changed = entry.status != status || entry.activity != activity;
        entry.status = status;
        entry.activity.clone_from(&activity);
        Ok(changed.then(|| presence_event(user, status, activity)))
    }

    fn get(&self, user: Id) -> v1::Presence {
        match self.users.get(&user) {
            Some(entry) if !entry.conns.is_empty() => v1::Presence {
                status: entry.status,
                activity: entry.activity.clone(),
            },
            _ => offline_presence(),
        }
    }

    fn connection_ids(&self, user: Id) -> Vec<Id> {
        self.users
            .get(&user)
            .map_or_else(Vec::new, |entry| entry.conns.clone())
    }
}

struct Peer {
    connection: quinn::Connection,
    outbound: mpsc::Sender<v1::ServerEnvelope>,
    direct: Arc<DirectQueue>,
    kicks: mpsc::Sender<Kick>,
    client: ClientInfo,
    rtt: Option<Duration>,
    /// Voice connections this peer was told are in a channel.
    voice_seen: HashSet<Id>,
}

struct Inner {
    book: PresenceBook,
    peers: HashMap<Id, Peer>,
    slow_after: Duration,
}

/// `conn_id →` handle and `user_id →` live connections.
pub struct ConnectionRegistry {
    inner: Mutex<Inner>,
}

impl ConnectionRegistry {
    /// Internal operator diagnostics. No client-supplied strings, tokens or addresses.
    pub fn diagnostics(&self) -> Vec<serde_json::Value> {
        let inner = self.inner.lock();
        let mut rows: Vec<_> = inner.peers.iter().map(|(id,peer)| {
            let path = peer.connection.stats().path;
            serde_json::json!({"connectionId":id.to_string(),"rttMs":peer.connection.rtt().as_secs_f64()*1000.0,
                "lostPackets":path.lost_packets,"sentPackets":path.sent_packets,
                "congestionWindowBytes":path.cwnd,"pathMtuBytes":path.current_mtu,
                "queuedControlMessages":peer.outbound.max_capacity()-peer.outbound.capacity()
                    +peer.direct.len()})
        }).collect();
        rows.sort_by(|a, b| a["connectionId"].as_str().cmp(&b["connectionId"].as_str()));
        rows
    }

    pub(crate) fn new(slow_after: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner {
                book: PresenceBook::default(),
                peers: HashMap::new(),
                slow_after,
            }),
        }
    }

    /// Inserts the connection. The returned event is the user's first
    /// `PresenceUpdated` of this process, including a remembered idle or
    /// do-not-disturb status.
    pub(crate) fn register(&self, registration: Registration) -> Option<v1::server_envelope::Msg> {
        let mut inner = self.inner.lock();
        let event = inner
            .book
            .connect(registration.user_id, registration.conn_id);
        inner.peers.insert(
            registration.conn_id,
            Peer {
                connection: registration.connection,
                outbound: registration.outbound,
                direct: registration.direct,
                kicks: registration.kicks,
                client: registration.client,
                rtt: None,
                voice_seen: HashSet::new(),
            },
        );
        event
    }

    /// Records the voice connections a `Welcome` snapshot showed `conn`.
    pub(crate) fn seed_voice(&self, conn: Id, seen: impl IntoIterator<Item = Id>) {
        if let Some(peer) = self.inner.lock().peers.get_mut(&conn) {
            peer.voice_seen.extend(seen);
        }
    }

    /// Whether `frame`, already cleared for channel visibility, may go to
    /// `conn` as far as voice state is concerned.
    ///
    /// A `VoiceStateUpdated` naming a channel is remembered and passes. One
    /// naming no channel passes only once for a voice connection `conn` was
    /// told about, so a leave from a channel `conn` cannot see stays quiet.
    /// Other frames pass. [`Self::enqueue`] applies this itself.
    pub(crate) fn admit_voice(&self, conn: Id, frame: &v1::ServerEnvelope) -> bool {
        self.inner
            .lock()
            .peers
            .get_mut(&conn)
            .is_some_and(|peer| admit(&mut peer.voice_seen, frame))
    }

    /// Drops the connection. The returned event is offline when it was the
    /// user's last one.
    pub(crate) fn unregister(&self, conn: Id) -> Option<v1::server_envelope::Msg> {
        let mut inner = self.inner.lock();
        inner.peers.remove(&conn);
        inner.book.disconnect(conn)
    }

    /// Updates the user's chosen status. Offline is not stored here.
    pub(crate) fn set_status(
        &self,
        user: Id,
        status: i32,
        activity: String,
    ) -> Result<Option<v1::server_envelope::Msg>, &'static str> {
        self.inner.lock().book.set(user, status, activity)
    }

    /// Live presence, or offline when the user has no connection.
    pub fn presence(&self, user: Id) -> v1::Presence {
        self.inner.lock().book.get(user)
    }

    /// Connections currently registered for `user`.
    pub fn connection_ids(&self, user: Id) -> Vec<Id> {
        self.inner.lock().book.connection_ids(user)
    }

    /// Registered connections.
    pub fn connection_count(&self) -> usize {
        self.inner.lock().peers.len()
    }

    /// `Hello` product strings.
    pub fn client_info(&self, conn: Id) -> Option<ClientInfo> {
        self.inner
            .lock()
            .peers
            .get(&conn)
            .map(|peer| peer.client.clone())
    }

    /// Latest QUIC RTT sampled from a `Ping`.
    pub fn rtt(&self, conn: Id) -> Option<Duration> {
        self.inner.lock().peers.get(&conn).and_then(|peer| peer.rtt)
    }

    /// Stores Quinn's current RTT estimate for diagnostics.
    pub(crate) fn record_rtt(&self, conn: Id) {
        let mut inner = self.inner.lock();
        let Some(peer) = inner.peers.get_mut(&conn) else {
            return;
        };
        peer.rtt = Some(peer.connection.rtt());
    }

    /// How long one control-stream write may stall before the connection is
    /// closed. The server uses [`SLOW_CONSUMER_AFTER`]; tests change it.
    pub fn set_slow_after(&self, slow_after: Duration) {
        self.inner.lock().slow_after = slow_after;
    }

    /// Current slow-consumer grace.
    pub fn slow_after(&self) -> Duration {
        self.inner.lock().slow_after
    }

    /// Queues a frame for `conn` from outside its session loop. A queue that
    /// has stayed past [`OUTBOUND_CAPACITY`] for [`Self::slow_after`], or
    /// reached [`DIRECT_LIMIT`], closes that connection with a
    /// [`v1::ServerNotice`] instead of dropping the frame, so the client
    /// resyncs from `Welcome`.
    pub fn enqueue(&self, conn: Id, frame: v1::ServerEnvelope) {
        let kick = {
            let mut inner = self.inner.lock();
            let slow_after = inner.slow_after;
            let Some(peer) = inner.peers.get_mut(&conn) else {
                return;
            };
            if !admit(&mut peer.voice_seen, &frame) {
                return;
            }
            (!peer.direct.push(frame, slow_after)).then(|| peer.kick_pair())
        };
        if let Some((kicks, connection)) = kick {
            crate::observe::record_slow_disconnect();
            kick_peer(
                &kicks,
                &connection,
                CLOSE_SLOW,
                SLOW_REASON,
                notice(SLOW_NOTICE),
            );
        }
    }

    /// Disconnects `conn` for falling behind the event bus.
    pub(crate) fn kick_slow(&self, conn: Id) {
        let Some((kicks, connection)) = self.kick_pair(conn) else {
            return;
        };
        crate::observe::record_slow_disconnect();
        kick_peer(
            &kicks,
            &connection,
            CLOSE_SLOW,
            SLOW_REASON,
            notice(SLOW_NOTICE),
        );
    }

    /// Sends `notice` and closes every live connection for `user`.
    pub(crate) fn disconnect_user(
        &self,
        user: Id,
        notice: &v1::ServerEnvelope,
        code: u32,
        reason: &'static [u8],
    ) {
        let peers: Vec<_> = {
            let inner = self.inner.lock();
            inner
                .book
                .connection_ids(user)
                .into_iter()
                .filter_map(|conn| inner.peers.get(&conn).map(Peer::kick_pair))
                .collect()
        };
        for (kicks, connection) in peers {
            kick_peer(&kicks, &connection, code, reason, notice.clone());
        }
    }

    /// Sends a shutdown notice and closes every connection with
    /// [`CLOSE_SHUTDOWN`].
    pub fn shutdown(&self) {
        let peers: Vec<_> = {
            let inner = self.inner.lock();
            inner.peers.values().map(Peer::kick_pair).collect()
        };
        for (kicks, connection) in peers {
            kick_peer(
                &kicks,
                &connection,
                CLOSE_SHUTDOWN,
                SHUTDOWN_REASON,
                notice(SHUTDOWN_NOTICE),
            );
        }
    }

    fn kick_pair(&self, conn: Id) -> Option<(mpsc::Sender<Kick>, quinn::Connection)> {
        self.inner.lock().peers.get(&conn).map(Peer::kick_pair)
    }
}

/// Frames other tasks address to one connection.
///
/// Unlike an mpsc channel it can replace a pending per-track frame and hold
/// frames past its nominal size for a grace period.
#[derive(Default)]
pub struct DirectQueue {
    state: Mutex<DirectState>,
    ready: tokio::sync::Notify,
}

#[derive(Default)]
struct DirectState {
    frames: VecDeque<v1::ServerEnvelope>,
    /// When the queue last reached [`OUTBOUND_CAPACITY`] without draining
    /// below it.
    full_since: Option<Instant>,
}

impl DirectQueue {
    /// Adds `frame`, or replaces the pending frame for the same track.
    /// `false` means the connection must be closed: the queue has been full
    /// for `slow_after`, or holds [`DIRECT_LIMIT`] frames.
    fn push(&self, frame: v1::ServerEnvelope, slow_after: Duration) -> bool {
        let mut state = self.state.lock();
        if let Some(key) = track_key(&frame)
            && let Some(pending) = state
                .frames
                .iter_mut()
                .find(|pending| track_key(pending) == Some(key))
        {
            *pending = frame;
            return true;
        }
        let len = state.frames.len();
        if len >= DIRECT_LIMIT {
            return false;
        }
        if len >= OUTBOUND_CAPACITY {
            let since = *state.full_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= slow_after {
                return false;
            }
        }
        state.frames.push_back(frame);
        drop(state);
        self.ready.notify_one();
        true
    }

    /// The oldest pending frame. The grace period ends once the queue drains
    /// below [`OUTBOUND_CAPACITY`].
    pub(crate) fn pop(&self) -> Option<v1::ServerEnvelope> {
        let mut state = self.state.lock();
        let frame = state.frames.pop_front();
        if state.frames.len() < OUTBOUND_CAPACITY {
            state.full_since = None;
        }
        frame
    }

    /// Resolves after a push. Check [`Self::pop`] before waiting: a push
    /// made while nobody waits leaves one stored wake-up.
    pub(crate) async fn ready(&self) {
        self.ready.notified().await;
    }

    fn len(&self) -> usize {
        self.state.lock().frames.len()
    }
}

/// Frames for which only the newest per track matters.
fn track_key(frame: &v1::ServerEnvelope) -> Option<(u8, u32)> {
    match &frame.msg {
        Some(v1::server_envelope::Msg::SubscriberHint(hint)) => Some((0, hint.track_id)),
        // Each rendition keeps its own pending keyframe request.
        Some(v1::server_envelope::Msg::RequestKeyframe(request)) => {
            Some((if request.rendition == 0 { 1 } else { 3 }, request.track_id))
        }
        Some(v1::server_envelope::Msg::SubscriberStats(stats)) => Some((2, stats.track_id)),
        _ => None,
    }
}

impl Peer {
    fn kick_pair(&self) -> (mpsc::Sender<Kick>, quinn::Connection) {
        (self.kicks.clone(), self.connection.clone())
    }
}

fn kick_peer(
    kicks: &mpsc::Sender<Kick>,
    connection: &quinn::Connection,
    code: u32,
    reason: &'static [u8],
    frame: v1::ServerEnvelope,
) {
    let kick = Kick {
        notice: frame,
        code,
        reason,
    };
    if kicks.try_send(kick).is_err() {
        connection.close(code.into(), reason);
    }
}

pub(crate) fn notice(text: impl Into<String>) -> v1::ServerEnvelope {
    v1::ServerEnvelope {
        request_id: 0,
        msg: Some(v1::server_envelope::Msg::ServerNotice(v1::ServerNotice {
            text: text.into(),
        })),
    }
}

fn offline_presence() -> v1::Presence {
    v1::Presence {
        status: i32::from(v1::PresenceStatus::Offline),
        activity: String::new(),
    }
}

fn presence_event(user: Id, status: i32, activity: String) -> v1::server_envelope::Msg {
    v1::server_envelope::Msg::PresenceUpdated(v1::PresenceUpdated {
        user_id: id_to_bytes(user),
        status,
        activity,
    })
}

fn admit(seen: &mut HashSet<Id>, frame: &v1::ServerEnvelope) -> bool {
    let Some(v1::server_envelope::Msg::VoiceStateUpdated(state)) = &frame.msg else {
        return true;
    };
    let Ok(conn) = ss_proto::validate::id_from_bytes(&state.conn_id) else {
        return false;
    };
    if state.channel_id.is_some() {
        seen.insert(conn);
        true
    } else {
        seen.remove(&conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_connections_emit_online_once_and_offline_on_the_last_close() {
        let mut book = PresenceBook::default();
        let user = Id::generate();
        let first = Id::generate();
        let second = Id::generate();
        let online = book.connect(user, first).expect("first connection");
        assert!(matches!(
            online,
            v1::server_envelope::Msg::PresenceUpdated(ref event)
                if event.status == i32::from(v1::PresenceStatus::Online)
        ));
        assert!(book.connect(user, second).is_none());
        assert!(book.disconnect(second).is_none());
        let offline = book.disconnect(first).expect("last connection");
        assert!(matches!(
            offline,
            v1::server_envelope::Msg::PresenceUpdated(ref event)
                if event.status == i32::from(v1::PresenceStatus::Offline)
                    && event.activity.is_empty()
        ));
        assert_eq!(
            book.get(user).status,
            i32::from(v1::PresenceStatus::Offline)
        );
    }

    fn note(text: &str) -> v1::ServerEnvelope {
        notice(text)
    }

    #[test]
    fn a_direct_queue_holds_past_capacity_for_the_grace_then_refuses() {
        let queue = DirectQueue::default();
        let grace = Duration::from_millis(50);
        for n in 0..OUTBOUND_CAPACITY + 10 {
            assert!(
                queue.push(note(&n.to_string()), grace),
                "held during the grace"
            );
        }
        std::thread::sleep(grace * 2);
        assert!(
            !queue.push(note("late"), grace),
            "still full after the grace"
        );
        // Draining below capacity ends the grace period.
        while queue.len() >= OUTBOUND_CAPACITY {
            let _ = queue.pop();
        }
        assert!(queue.push(note("fresh"), grace));
        assert_eq!(queue.pop().map(|frame| frame.msg), Some(note("11").msg));
    }

    #[test]
    fn a_direct_queue_never_grows_past_its_limit() {
        let queue = DirectQueue::default();
        let forever = Duration::from_secs(3600);
        for n in 0..DIRECT_LIMIT {
            assert!(queue.push(note(&n.to_string()), forever));
        }
        assert!(!queue.push(note("one more"), forever));
    }

    #[test]
    fn chosen_status_is_restored_on_reconnect() {
        let mut book = PresenceBook::default();
        let user = Id::generate();
        let first = Id::generate();
        book.connect(user, first);
        let idle = book
            .set(user, i32::from(v1::PresenceStatus::Idle), "lunch".into())
            .unwrap()
            .expect("status change");
        assert!(matches!(
            idle,
            v1::server_envelope::Msg::PresenceUpdated(ref event)
                if event.status == i32::from(v1::PresenceStatus::Idle)
                    && event.activity == "lunch"
        ));
        assert!(
            book.set(user, i32::from(v1::PresenceStatus::Idle), "lunch".into())
                .unwrap()
                .is_none()
        );
        book.disconnect(first);
        let again = book.connect(user, Id::generate()).expect("reconnect");
        assert!(matches!(
            again,
            v1::server_envelope::Msg::PresenceUpdated(ref event)
                if event.status == i32::from(v1::PresenceStatus::Idle)
                    && event.activity == "lunch"
        ));
    }
}
