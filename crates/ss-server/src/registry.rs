//! Authenticated connections and the presence derived from them.
//!
//! A user is online while at least one connection is registered. The first
//! connection emits `PresenceUpdated`; the last disconnect emits offline.
//! Explicit idle and do-not-disturb survive a reconnect until the process
//! restarts.

use std::{
    collections::HashMap,
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
/// How long a full outbound queue, or one stalled write, is tolerated.
pub const SLOW_CONSUMER_AFTER: Duration = Duration::from_secs(10);
/// Per-connection outbound control queue. Command replies wait for space.
/// Fan-out uses `try_send` and starts the slow-consumer timer when it is full.
pub const OUTBOUND_CAPACITY: usize = 32;

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
    /// Bounded control queue. The session writer owns the receiver.
    pub outbound: mpsc::Sender<v1::ServerEnvelope>,
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
    kicks: mpsc::Sender<Kick>,
    client: ClientInfo,
    rtt: Option<Duration>,
    blocked_since: Option<Instant>,
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
                "queuedControlMessages":peer.outbound.max_capacity()-peer.outbound.capacity()})
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
                kicks: registration.kicks,
                client: registration.client,
                rtt: None,
                blocked_since: None,
            },
        );
        event
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

    /// Grace period before a wedged consumer is closed. The server uses
    /// [`SLOW_CONSUMER_AFTER`]; tests shorten it.
    pub fn set_slow_after(&self, slow_after: Duration) {
        self.inner.lock().slow_after = slow_after;
    }

    /// Current slow-consumer grace.
    pub fn slow_after(&self) -> Duration {
        self.inner.lock().slow_after
    }

    /// Queues a fan-out frame. A queue that stays full for [`Self::slow_after`]
    /// disconnects that connection with a [`v1::ServerNotice`].
    pub fn enqueue(&self, conn: Id, frame: v1::ServerEnvelope) {
        let kick = {
            let mut inner = self.inner.lock();
            let slow_after = inner.slow_after;
            let Some(peer) = inner.peers.get_mut(&conn) else {
                return;
            };
            match peer.outbound.try_send(frame) {
                Ok(()) => {
                    peer.blocked_since = None;
                    None
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let start = peer.blocked_since.unwrap_or_else(|| {
                        let now = Instant::now();
                        peer.blocked_since = Some(now);
                        now
                    });
                    (start.elapsed() >= slow_after).then(|| peer.kick_pair())
                }
                Err(mpsc::error::TrySendError::Closed(_)) => None,
            }
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
