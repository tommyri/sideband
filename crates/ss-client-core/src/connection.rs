//! Connection ownership, authentication, bounded commands, and session resume.

use crate::{
    ClientEvent, ConnectionState, Error, StateStore,
    storage::{MemoryStorage, Storage},
};
use bytes::Bytes;
use parking_lot::RwLock;
use ss_net::{
    control::{ClientControl, Reply},
    pins::{PinStore, ServerId},
};
use ss_proto::v1::{self, client_envelope::Msg, server_envelope::Msg as Event};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, timeout},
};

pub(crate) const DEADLINE: Duration = Duration::from_secs(10);
const RECONNECT_CLOSE_CODE: u32 = 73;
/// Server application close codes (`02-protocol.md` §1.1) after which the
/// client does not reconnect, whether or not the event sent in front of the
/// close (`Kicked`, `Banned`, `SessionRevoked`) arrived first.
const CLOSE_KICKED: u32 = 3;
const CLOSE_BANNED: u32 = 4;
const CLOSE_SIGNED_OUT: u32 = 6;
/// Server shutting down: reconnect, starting the backoff over.
const CLOSE_SHUTDOWN: u32 = 1;
/// Media rate limit: reconnect, but do not count the session as stable.
const CLOSE_RATE_LIMITED: u32 = 5;
/// Message sends queued while reconnecting, at most. Further sends fail
/// with `Capacity` until the connection is back.
const MAX_PENDING: usize = 100;
/// A session that lasted this long resets the reconnect backoff. Shorter
/// ones keep climbing it, so a server that drops every resume at once is
/// not hammered every two seconds.
const STABLE_SESSION: Duration = Duration::from_secs(30);
/// Longest server-stated wait (`retry_after_ms`) the client honours as given.
const MAX_STATED_WAIT: Duration = Duration::from_mins(10);
/// A send whose Ack never came is replayed with its nonce only this long
/// after it first went out. The server deduplicates nonces for five
/// minutes; after that a replay could store the message twice, so its
/// caller gets `Timeout` (outcome unknown) instead. Sends queued while
/// offline never went out and are not limited.
const REPLAY_WINDOW: Duration = Duration::from_mins(4);
/// Pages fetched per channel when resyncing after a reconnect.
const RESYNC_PAGES: usize = 4;
/// `429` refusals one resync page waits out before its channel is deferred.
const RESYNC_RATE_LIMITS: u32 = 3;

/// One host, QUIC port, and HTTPS port. HTTPS uses the QUIC identity's pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAddr {
    /// Bare DNS hostname or IP address (IPv6 brackets are accepted).
    pub host: String,
    /// QUIC UDP port.
    pub port: u16,
    /// HTTPS TCP port; normally the same numeric port.
    pub https_port: u16,
}
impl ServerAddr {
    /// Construct and validate the host/port identity.
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, Error> {
        let host = host
            .into()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        ServerId::new(&host, port)?;
        Ok(Self {
            host,
            port,
            https_port: port,
        })
    }
    /// Normalized key for persistence and multi-server ownership.
    pub fn id(&self) -> Result<ServerId, Error> {
        Ok(ServerId::new(&self.host, self.port)?)
    }
}

/// Authentication material; deliberately does not implement Debug.
pub enum Credentials {
    /// Load a saved session from the host's Storage implementation.
    Saved,
    /// Authenticate with a previously issued token.
    Session(String),
    /// Password login.
    Login(v1::Login),
    /// Invite registration.
    Register(v1::Register),
}

pub(crate) struct Command {
    pub message: Msg,
    pub generation: u64,
    pub replayable: bool,
    pub reply: oneshot::Sender<Result<v1::Ack, Error>>,
}

#[derive(Debug, Clone)]
pub(crate) struct IncomingMedia {
    pub bytes: Bytes,
    pub at: std::time::Instant,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TransportStats {
    pub rtt: Duration,
    pub lost_packets: u64,
}

#[derive(Clone)]
pub(crate) struct MediaLink {
    pub(crate) video: Arc<parking_lot::Mutex<crate::media::viewer::Hub>>,
    pub(crate) audio_clocks: Arc<parking_lot::Mutex<BTreeMap<u32, crate::media::AudioClock>>>,
    pub(crate) capture_clock: Arc<parking_lot::Mutex<Option<crate::media::AudioClock>>>,
    outgoing: broadcast::Sender<Bytes>,
    incoming: broadcast::Sender<IncomingMedia>,
    stats: Arc<RwLock<TransportStats>>,
    quic: Arc<RwLock<Option<quinn::Connection>>>,
}

impl MediaLink {
    pub(crate) fn new() -> Self {
        let (outgoing, _) = broadcast::channel(256);
        let (incoming, _) = broadcast::channel(512);
        Self {
            video: Arc::default(),
            audio_clocks: Arc::default(),
            capture_clock: Arc::default(),
            outgoing,
            incoming,
            stats: Arc::new(RwLock::new(TransportStats::default())),
            quic: Arc::new(RwLock::new(None)),
        }
    }

    pub fn send(&self, packet: Bytes) {
        let _ = self.outgoing.send(packet);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<IncomingMedia> {
        self.incoming.subscribe()
    }

    pub fn stats(&self) -> TransportStats {
        *self.stats.read()
    }

    pub fn quic(&self) -> Option<quinn::Connection> {
        self.quic.read().clone()
    }

    fn connected(&self, connection: Option<&ss_net::Connection>) {
        *self.quic.write() = connection.map(|value| value.quic().clone());
    }

    /// Datagram I/O and path stats for one connection, on a task of its own.
    /// A command awaiting its Ack, a reconnect step, or a message resync never
    /// delays audio, and each arrival is stamped as it leaves QUIC. Ends when
    /// the connection closes; `Live` also aborts it when dropped.
    fn pump(&self, quic: quinn::Connection) -> JoinHandle<()> {
        let mut outgoing = self.outgoing.subscribe();
        let incoming = self.incoming.clone();
        let stats = self.stats.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    packet = quic.read_datagram() => {
                        let Ok(bytes) = packet else { return };
                        let _ = incoming.send(IncomingMedia {
                            bytes,
                            at: std::time::Instant::now(),
                        });
                    }
                    packet = outgoing.recv() => match packet {
                        Ok(packet) => { let _ = quic.send_datagram(packet); }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return,
                    },
                    _ = tick.tick() => {
                        *stats.write() = TransportStats {
                            rtt: quic.rtt(),
                            lost_packets: quic.stats().path.lost_packets,
                        };
                    }
                }
            }
        })
    }
}

/// Credentials and the trust decision verified by their QUIC connection.
#[derive(Clone)]
pub(crate) struct AuthSession {
    pub token: String,
    pub trust: ss_net::pins::TrustPolicy,
}

/// One independently owned client. Drop aborts its worker, which drops the
/// control router and closes QUIC. No worker retains an Arc back to Client.
pub struct Client {
    pub(crate) commands: mpsc::Sender<Command>,
    pub(crate) store: StateStore,
    pub(crate) events: broadcast::Sender<ClientEvent>,
    pub(crate) auth: Arc<RwLock<Option<AuthSession>>>,
    pub(crate) address: ServerAddr,
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) media: MediaLink,
    #[cfg(test)]
    pub(crate) clock: Clock,
    worker: JoinHandle<()>,
}

impl Client {
    /// Connect without saving sessions across process lifetimes. An untrusted
    /// or changed certificate returns the ss-net error before credentials leave.
    pub async fn connect(
        address: ServerAddr,
        credentials: Credentials,
        pins: Arc<dyn PinStore>,
    ) -> Result<Self, Error> {
        Self::connect_with_storage(
            address,
            credentials,
            pins,
            Arc::new(MemoryStorage::default()),
        )
        .await
    }

    /// Connect with host persistence. Does not retain a password after Welcome.
    pub async fn connect_with_storage(
        address: ServerAddr,
        credentials: Credentials,
        pins: Arc<dyn PinStore>,
        storage: Arc<dyn Storage>,
    ) -> Result<Self, Error> {
        let key = address.id()?;
        let credential = match credentials {
            Credentials::Saved => {
                let storage = storage.clone();
                let token = tokio::task::spawn_blocking(move || storage.session(&key))
                    .await
                    .map_err(|e| Error::Storage(e.to_string()))??
                    .ok_or(Error::Disconnected)?;
                v1::hello::Auth::SessionToken(token)
            }
            Credentials::Session(token) => v1::hello::Auth::SessionToken(token),
            Credentials::Login(login) => v1::hello::Auth::Login(login),
            Credentials::Register(register) => v1::hello::Auth::Register(register),
        };
        let (live, welcome) = establish(&address, pins.clone(), credential).await?;
        let store = StateStore::default();
        store.welcome(&welcome)?;
        persist(&storage, &address, &welcome.resume_token).await?;
        let auth = Arc::new(RwLock::new(Some(AuthSession {
            token: welcome.resume_token,
            trust: live
                .connection
                .trust_policy()
                .ok_or(Error::Protocol("missing verified trust"))?,
        })));
        let (events, _) = broadcast::channel(256);
        let (commands, command_rx) = mpsc::channel(64);
        let media = MediaLink::new();
        let clock = Clock::default();
        let runner = Runner {
            address: address.clone(),
            pins: pins.clone(),
            storage: storage.clone(),
            store: store.clone(),
            events: events.clone(),
            auth: auth.clone(),
            desired_voice: None,
            typing: HashMap::new(),
            pending: VecDeque::new(),
            media: media.clone(),
            clock: clock.clone(),
        };
        let worker = tokio::spawn(runner.run(live, command_rx));
        Ok(Self {
            commands,
            store,
            events,
            auth,
            address,
            storage,
            media,
            #[cfg(test)]
            clock,
            worker,
        })
    }
    /// Subscribe, then read `state()` to establish a race-free UI baseline.
    pub fn subscribe(&self) -> broadcast::Receiver<ClientEvent> {
        self.events.subscribe()
    }
    /// Cloneable, read-only shared state handle.
    pub fn state(&self) -> StateStore {
        self.store.clone()
    }
    /// The configured server address.
    pub fn address(&self) -> &ServerAddr {
        &self.address
    }
    /// Close and wait for the owning worker to release its resources.
    pub async fn close(mut self) {
        self.worker.abort();
        let _ = (&mut self.worker).await;
    }
    /// Stop the worker without waiting, for hosts that share the client while
    /// commands await it. Pending commands fail with [`Error::Disconnected`]
    /// and no reconnect follows. Dropping the last owner finishes cleanup.
    pub fn abort(&self) {
        self.worker.abort();
    }

    /// Low-level command escape hatch. Hello/Ping are owned by the connection
    /// manager; application commands use correlated Ack/Error replies. Message
    /// sends carry an idempotency nonce and may be queued/replayed on reconnect.
    pub async fn command(&self, mut message: Msg) -> Result<v1::Ack, Error> {
        if matches!(message, Msg::Hello(_) | Msg::Ping(_)) {
            return Err(Error::Protocol("connection-owned command"));
        }
        let replayable = make_replayable(&mut message);
        let generation = {
            let state = self.store.read();
            if state.connection != ConnectionState::Connected
                && !(replayable
                    && matches!(&state.connection, ConnectionState::Reconnecting { .. }))
            {
                return Err(Error::Disconnected);
            }
            state.generation
        };
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command {
                message,
                generation,
                replayable,
                reply,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Closed(_) => Error::Disconnected,
                mpsc::error::TrySendError::Full(_) => {
                    Error::Control(ss_net::control::ControlError::Capacity)
                }
            })?;
        response.await.map_err(|_| Error::Disconnected)?
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.worker.abort();
        *self.auth.write() = None;
        self.store.connection(ConnectionState::Disconnected);
        let _ = self
            .events
            .send(ClientEvent::Connection(ConnectionState::Disconnected));
    }
}

/// Owns several independent clients. Removing/dropping a client closes it.
#[derive(Default)]
pub struct ClientSet(BTreeMap<ServerId, Client>);
impl ClientSet {
    /// Add a connected client. Returns the previous owner if replacing one.
    pub fn insert(&mut self, client: Client) -> Result<Option<Client>, Error> {
        Ok(self.0.insert(client.address.id()?, client))
    }
    /// Find a connection by normalized server identity.
    pub fn get(&self, server: &ServerId) -> Option<&Client> {
        self.0.get(server)
    }
    /// Remove a client, transferring ownership to the caller.
    pub fn remove(&mut self, server: &ServerId) -> Option<Client> {
        self.0.remove(server)
    }
    /// Iterate active server ids and clients.
    pub fn iter(&self) -> impl Iterator<Item = (&ServerId, &Client)> {
        self.0.iter()
    }
}

struct Live {
    connection: ss_net::Connection,
    control: Option<ClientControl>,
    incoming: mpsc::Receiver<v1::ServerEnvelope>,
    /// Media pump task, started once this connection may carry voice.
    media: Option<JoinHandle<()>>,
}
impl Drop for Live {
    fn drop(&mut self) {
        if let Some(pump) = &self.media {
            pump.abort();
        }
        self.connection.close(0, b"client session ended");
    }
}
impl Live {
    fn control(&self) -> &ClientControl {
        self.control
            .as_ref()
            .expect("establish installs the router before returning Live")
    }
    fn start_media(&mut self, link: &MediaLink) {
        if self.media.is_none() {
            self.media = Some(link.pump(self.connection.quic().clone()));
        }
    }
}

async fn establish(
    address: &ServerAddr,
    pins: Arc<dyn PinStore>,
    auth: v1::hello::Auth,
) -> Result<(Live, v1::Welcome), Error> {
    timeout(DEADLINE, async {
        let connection = ss_net::client::connect(&address.host, address.port, pins).await?;
        let mut live = Live {
            connection,
            control: None,
            incoming: mpsc::channel(1).1,
            media: None,
        };
        let (control, incoming) = ClientControl::start(live.connection.open_control().await?);
        live.control = Some(control);
        live.incoming = incoming;
        let response = live
            .control()
            .request(Msg::Hello(v1::Hello {
                protocol_version: ss_proto::PROTOCOL_VERSION,
                client_name: "Sideband".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                os: std::env::consts::OS.into(),
                auth: Some(auth),
            }))
            .await?;
        match response.msg {
            Some(Event::Welcome(welcome)) if !welcome.resume_token.is_empty() => {
                Ok((live, welcome))
            }
            Some(Event::Error(error)) => Err(error.into()),
            _ => Err(Error::Protocol(
                "expected authenticated Welcome with session token",
            )),
        }
    })
    .await
    .map_err(|_| Error::Timeout)?
}
async fn persist(
    storage: &Arc<dyn Storage>,
    address: &ServerAddr,
    token: &str,
) -> Result<(), Error> {
    let storage = storage.clone();
    let address = address.clone();
    let token = token.to_owned();
    tokio::task::spawn_blocking(move || {
        storage.save_session(&address.id()?, &token)?;
        storage.save_last_server(&address)
    })
    .await
    .map_err(|e| Error::Storage(e.to_string()))?
}

fn is_new_live_message(view: &crate::State, message: &v1::Message) -> bool {
    let Ok(channel) = ss_core::Id::try_from(message.channel_id.as_ref()) else {
        return false;
    };
    // A message a resync page brought in before its own live event is still
    // new: the page is read from the database and can overtake the event.
    let unannounced = |cache: &crate::state::MessageCache| {
        ss_core::Id::try_from(message.id.as_ref()).is_ok_and(|key| cache.unannounced(key))
    };
    !view.messages.get(&channel).is_some_and(|cache| {
        cache
            .ordered()
            .iter()
            .chain(cache.deleted().iter())
            .any(|cached| cached.id == message.id)
            && !unannounced(cache)
    }) && view
        .channels
        .get(&channel)
        .or_else(|| view.direct_messages.get(&channel))
        .is_none_or(|channel| channel.last_message_id.as_ref() != Some(&message.id))
}

struct Runner {
    address: ServerAddr,
    pins: Arc<dyn PinStore>,
    storage: Arc<dyn Storage>,
    store: StateStore,
    events: broadcast::Sender<ClientEvent>,
    auth: Arc<RwLock<Option<AuthSession>>>,
    desired_voice: Option<v1::JoinVoice>,
    typing: HashMap<Vec<u8>, Instant>,
    pending: VecDeque<Pending>,
    media: MediaLink,
    clock: Clock,
}

/// A replayable command waiting for the connection. `sent` is when it
/// first went out; `None` if it never reached the server.
struct Pending {
    command: Command,
    sent: Option<ClockSample>,
}

/// The one command whose Ack the connection loop is waiting for. Commands run
/// one at a time, in order; the wait is a select branch like any other.
struct InFlight<'a> {
    command: Command,
    sent: ClockSample,
    reply: Pin<Box<dyn Future<Output = Result<v1::Ack, Error>> + Send + 'a>>,
}

impl Runner {
    fn status(&self, state: ConnectionState) {
        self.store.connection(state.clone());
        let _ = self.events.send(ClientEvent::Connection(state));
    }
    fn event(&mut self, event: v1::ServerEnvelope) -> Result<bool, Error> {
        let message = event.msg.ok_or(Error::Protocol("empty event"))?;
        if let Event::VoiceStateUpdated(update) = &message {
            self.follow_own_voice(update);
        }
        let received = match &message {
            v1::server_envelope::Msg::MessageCreated(created) => created
                .message
                .as_ref()
                .filter(|message| is_new_live_message(&self.store.read(), message))
                .cloned(),
            _ => None,
        };
        let event = self.store.apply(message)?;
        let removed = matches!(event, ClientEvent::Removed { .. });
        let _ = self.events.send(event);
        if let Some(message) = received
            && let (Ok(channel), Ok(message)) = (
                ss_core::Id::try_from(message.channel_id.as_ref()),
                ss_core::Id::try_from(message.id.as_ref()),
            )
        {
            let _ = self
                .events
                .send(ClientEvent::MessageReceived { channel, message });
        }
        Ok(removed)
    }
    async fn run(mut self, mut live: Live, mut commands: mpsc::Receiver<Command>) {
        live.start_media(&self.media);
        self.media.connected(Some(&live.connection));
        let mut resync = None;
        // Reconnect attempts since the last stable session; sets the backoff.
        let mut level = 0_u32;
        loop {
            let started = Instant::now();
            let outcome = self
                .connected(&mut live, &mut commands, resync.take())
                .await;
            // Read first: quinn's `close` replaces the peer's reason.
            let code = close_code(live.connection.quic());
            if outcome.as_ref().is_err_and(|error| !terminal(error)) {
                live.connection
                    .close(RECONNECT_CLOSE_CODE, b"client reconnecting");
            }
            // An announced shutdown starts the backoff over: the server is
            // restarting, not dropping this client. Otherwise only a stable
            // session does, and never a media rate-limit close.
            if code == Some(CLOSE_SHUTDOWN)
                || (started.elapsed() >= STABLE_SESSION && code != Some(CLOSE_RATE_LIMITED))
            {
                level = 0;
            }
            self.media.connected(None);
            drop(live);
            match outcome {
                Ok(()) => {
                    self.status(ConnectionState::Disconnected);
                    return;
                }
                Err(ref error) if terminal(error) => {
                    self.status(ConnectionState::Failed(error.to_string()));
                    return;
                }
                Err(_) => {}
            }
            let Some((next, next_resync)) = self.reconnect(&mut commands, &mut level).await else {
                return;
            };
            live = next;
            resync = next_resync;
        }
    }
    /// Resume the session until it works, a refusal is final, or the host
    /// drops the client. A `RATE_LIMITED` refusal waits as long as the
    /// server says; other failures back off from `level`.
    async fn reconnect(
        &mut self,
        commands: &mut mpsc::Receiver<Command>,
        level: &mut u32,
    ) -> Option<(Live, Option<Resync>)> {
        let session = self.auth.read().clone();
        let Some(session) = session else {
            self.status(ConnectionState::Disconnected);
            return None;
        };
        let mut token = session.token;
        let mut attempt = 0_u32;
        let mut stated = None;
        loop {
            attempt = attempt.saturating_add(1);
            *level = level.saturating_add(1);
            let delay = stated.take().map_or_else(|| backoff(*level), stated_delay);
            let reconnecting = ConnectionState::Reconnecting { attempt, delay };
            self.status(reconnecting.clone());
            while_reconnecting(tokio::time::sleep(delay), commands, &mut self.pending).await?;
            let result = while_reconnecting(
                establish(
                    &self.address,
                    self.pins.clone(),
                    v1::hello::Auth::ResumeToken(token.clone()),
                ),
                commands,
                &mut self.pending,
            )
            .await?;
            let error = match result {
                Ok((mut next, welcome)) => {
                    let floors = self.store.resync_floors();
                    match self
                        .resume(&mut next, &welcome, reconnecting, &mut token)
                        .await
                    {
                        Ok(trust) => {
                            let resync = self.reopened(&next, &welcome, trust, floors);
                            return Some((next, resync));
                        }
                        Err(error) => {
                            drop(next);
                            error
                        }
                    }
                }
                Err(error) => error,
            };
            if terminal(&error) {
                self.status(ConnectionState::Failed(error.to_string()));
                return None;
            }
            stated = match error {
                Error::Server { retry_after, .. } => retry_after,
                _ => None,
            };
        }
    }
    /// Install a resumed session and rejoin voice before `Connected`. The
    /// message resync is not part of this: it runs once connected.
    async fn resume(
        &mut self,
        next: &mut Live,
        welcome: &v1::Welcome,
        reconnecting: ConnectionState,
        token: &mut String,
    ) -> Result<ss_net::pins::TrustPolicy, Error> {
        self.store.install(welcome, reconnecting)?;
        let (storage, address) = (self.storage.clone(), self.address.clone());
        self.draining(
            &mut next.incoming,
            persist(&storage, &address, &welcome.resume_token),
        )
        .await?;
        token.clone_from(&welcome.resume_token);
        let trust = next
            .connection
            .trust_policy()
            .ok_or(Error::Protocol("missing verified trust"))?;
        *self.auth.write() = Some(AuthSession {
            token: welcome.resume_token.clone(),
            trust,
        });
        self.restore_voice(next).await?;
        // The seat is back: audio flows from here on, also while queued sends
        // replay.
        next.start_media(&self.media);
        self.flush_pending(next).await?;
        Ok(trust)
    }
    /// Report `Connected` for a resumed session and start fetching the
    /// messages missed while offline, alongside live events and audio.
    fn reopened(
        &mut self,
        next: &Live,
        welcome: &v1::Welcome,
        trust: ss_net::pins::TrustPolicy,
        floors: Vec<(ss_core::Id, ss_core::Id)>,
    ) -> Option<Resync> {
        self.typing.clear();
        let _ = self.events.send(ClientEvent::Resynced);
        self.status(ConnectionState::Connected);
        self.media.connected(Some(&next.connection));
        Resync::start(
            &self.address,
            trust,
            &welcome.resume_token,
            self.store.read().generation,
            floors,
            self.events.clone(),
        )
    }
    /// Serve one live connection until it ends. A message send whose Ack
    /// never arrived is queued for replay with its nonce.
    async fn connected(
        &mut self,
        live: &mut Live,
        commands: &mut mpsc::Receiver<Command>,
        mut resync: Option<Resync>,
    ) -> Result<(), Error> {
        let Live {
            connection,
            control,
            incoming,
            ..
        } = live;
        let control = control.as_ref().ok_or(Error::Disconnected)?;
        let quic = connection.quic().clone();
        let mut inflight = None;
        let outcome = self
            .serve(
                control,
                &quic,
                incoming,
                commands,
                &mut resync,
                &mut inflight,
            )
            .await;
        if let Some(InFlight { command, sent, .. }) = inflight
            && command.replayable
        {
            self.pending.push_front(Pending {
                command,
                sent: Some(sent),
            });
        }
        outcome
    }
    /// The connection loop. Every source is its own branch and no branch
    /// awaits another, so a slow Ack holds up neither events nor the
    /// heartbeat. Datagrams are not here at all: see [`MediaLink::pump`].
    async fn serve<'a>(
        &mut self,
        control: &'a ClientControl,
        quic: &quinn::Connection,
        incoming: &mut mpsc::Receiver<v1::ServerEnvelope>,
        commands: &mut mpsc::Receiver<Command>,
        resync: &mut Option<Resync>,
        inflight: &mut Option<InFlight<'a>>,
    ) -> Result<(), Error> {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_tick = self.clock.sample();
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    let now = self.clock.sample();
                    if clock_jumped(last_tick, now) {
                        return Err(Error::Disconnected);
                    }
                    last_tick = now;
                }
                _ = quic.closed() => return self.ended(quic, incoming),
                event = incoming.recv() => {
                    let Some(event) = event else { return self.ended(quic, incoming) };
                    if let Some(resync) = resync.as_mut() {
                        resync.observe(&event);
                    }
                    if self.event(event)? { return Ok(()); }
                }
                result = reply(inflight), if inflight.is_some() => {
                    if let Some(InFlight { command, sent, .. }) = inflight.take() {
                        self.finish(command, sent, result)?;
                    }
                }
                command = commands.recv(), if inflight.is_none() => {
                    let Some(command) = command else { return Ok(()); };
                    *inflight = self.begin(control, command);
                }
                page = next_page(resync), if resync.is_some() => match (page, resync.as_ref()) {
                    (Some(page), Some(active)) => {
                        let channel = page.channel;
                        let messages = active.reconcile(page.messages);
                        let live = |key: &ss_core::Id| active.live.contains_key(key);
                        if self
                            .store
                            .resync(active.generation, channel, page.span, messages, live)
                            .is_ok()
                        {
                            let _ = self.events.send(ClientEvent::MessagesChanged { channel });
                        }
                    }
                    _ => *resync = None,
                },
            }
        }
    }
    /// The connection is gone. Frames already read still count (a queued
    /// `Kicked` says more than a close code); then the server's close code
    /// tells a kick, ban or sign-out, which end the session, from a
    /// shutdown, slow-consumer or rate-limit close, which reconnect.
    fn ended(
        &mut self,
        quic: &quinn::Connection,
        incoming: &mut mpsc::Receiver<v1::ServerEnvelope>,
    ) -> Result<(), Error> {
        while let Ok(event) = incoming.try_recv() {
            if self.event(event)? {
                return Ok(());
            }
        }
        let (banned, reason) = match close_code(quic) {
            Some(CLOSE_KICKED) => (false, "Kicked from the server"),
            Some(CLOSE_BANNED) => (true, "Banned from the server"),
            Some(CLOSE_SIGNED_OUT) => (false, "Signed out"),
            _ => return Err(Error::Disconnected),
        };
        let _ = self.events.send(ClientEvent::Removed {
            banned,
            reason: reason.to_owned(),
        });
        Ok(())
    }
    /// Send `command`, or answer it at once when it is stale or throttled.
    fn begin<'a>(&self, control: &'a ClientControl, command: Command) -> Option<InFlight<'a>> {
        if command.reply.is_closed() {
            return None;
        }
        if command.generation != self.store.read().generation && !command.replayable {
            let _ = command.reply.send(Err(Error::Disconnected));
            return None;
        }
        if let Msg::Typing(typing) = &command.message
            && self
                .typing
                .get(typing.channel_id.as_ref())
                .is_some_and(|last| last.elapsed() < Duration::from_secs(5))
        {
            let _ = command.reply.send(Ok(v1::Ack {
                request_id: 0,
                result: Some(v1::ack::Result::Empty(v1::Empty {})),
            }));
            return None;
        }
        let reply = Box::pin(call(control, command.message.clone()));
        Some(InFlight {
            command,
            sent: self.clock.sample(),
            reply,
        })
    }
    /// Deliver a command's outcome. `Err` ends this connection.
    fn finish(
        &mut self,
        command: Command,
        sent: ClockSample,
        result: Result<v1::Ack, Error>,
    ) -> Result<(), Error> {
        if result.is_ok() {
            self.accepted(&command.message);
        }
        let stop = result
            .as_ref()
            .err()
            .is_some_and(|error| !matches!(error, Error::Server { .. }) && terminal(error));
        let retry = result
            .as_ref()
            .err()
            .is_some_and(|e| !matches!(e, Error::Server { .. }));
        if retry && command.replayable {
            self.pending.push_back(Pending {
                command,
                sent: Some(sent),
            });
            return Err(Error::Disconnected);
        }
        let _ = command.reply.send(result);
        if stop {
            return Err(Error::Protocol("command ended the session"));
        }
        if retry {
            return Err(Error::Disconnected);
        }
        Ok(())
    }
    /// Run one command during reconnect setup, before the connection loop.
    /// Audio is unaffected because the media pump is its own task.
    async fn execute(&mut self, live: &mut Live, message: Msg) -> Result<v1::Ack, Error> {
        let Live {
            control, incoming, ..
        } = live;
        let control = control.as_ref().ok_or(Error::Disconnected)?;
        self.draining(incoming, call(control, message)).await
    }
    /// Await one reconnect-setup step while applying events. The router
    /// waits for room when its event queue is full, and a reply behind those
    /// events waits with them: a step that awaited a reply without draining
    /// would stall until the server closed it as a slow consumer.
    async fn draining<T>(
        &mut self,
        incoming: &mut mpsc::Receiver<v1::ServerEnvelope>,
        step: impl Future<Output = Result<T, Error>>,
    ) -> Result<T, Error> {
        tokio::pin!(step);
        loop {
            tokio::select! {
                result = &mut step => return result,
                event = incoming.recv() => {
                    if self.event(event.ok_or(Error::Disconnected)?)? {
                        return Err(Error::Protocol("session removed by server"));
                    }
                }
            }
        }
    }
    async fn flush_pending(&mut self, live: &mut Live) -> Result<(), Error> {
        while let Some(Pending { command, sent }) = self.pending.pop_front() {
            if command.reply.is_closed() {
                continue;
            }
            let now = self.clock.sample();
            if sent.is_some_and(|sent| since(sent, now) > REPLAY_WINDOW) {
                // The server may have stored it and forgotten the nonce.
                let _ = command.reply.send(Err(Error::Timeout));
                continue;
            }
            let result = self.execute(live, command.message.clone()).await;
            if result
                .as_ref()
                .err()
                .is_some_and(|error| !matches!(error, Error::Server { .. }))
            {
                self.pending.push_front(Pending {
                    command,
                    sent: Some(sent.unwrap_or(now)),
                });
                return Err(Error::Disconnected);
            }
            if result.is_ok() {
                self.accepted(&command.message);
            }
            let _ = command.reply.send(result);
        }
        Ok(())
    }
    /// Rejoin voice after a reconnect, unless the server kept this
    /// connection's seat (a resumed lease) or the account's seat is now on
    /// another device. The server gives a user one seat, so joining again
    /// would take it back from that device.
    async fn restore_voice(&mut self, live: &mut Live) -> Result<(), Error> {
        let Some(join) = self.desired_voice.clone() else {
            return Ok(());
        };
        match self.resumed_seat(&join) {
            ResumedSeat::Kept(seat) => {
                self.desired_voice = Some(seat);
                return Ok(());
            }
            ResumedSeat::OtherDevice => {
                self.desired_voice = None;
                let _ = self.events.send(ClientEvent::Notice(
                    "Voice continued on another device".to_owned(),
                ));
                return Ok(());
            }
            ResumedSeat::Gone => {}
        }
        match self.execute(live, Msg::JoinVoice(join)).await {
            Ok(_) => Ok(()),
            Err(Error::Server { message, .. }) => {
                self.desired_voice = None;
                let _ = self.events.send(ClientEvent::Notice(format!(
                    "Could not rejoin voice: {message}"
                )));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    /// Where the account's voice seat is in a freshly installed snapshot.
    fn resumed_seat(&self, join: &v1::JoinVoice) -> ResumedSeat {
        let state = self.store.read();
        let Some(user) = state.own_user.as_ref().map(|user| &user.id) else {
            return ResumedSeat::Gone;
        };
        let seated = |voice: &&v1::VoiceState| voice.user_id == *user && voice.channel_id.is_some();
        let Some(conn) = state.own_conn else {
            // Older servers do not say which connection is ours: match the
            // user and channel only.
            return if state
                .voice_states
                .values()
                .filter(seated)
                .any(|voice| voice.channel_id.as_ref() == Some(&join.channel_id))
            {
                ResumedSeat::Kept(join.clone())
            } else {
                ResumedSeat::Gone
            };
        };
        if let Some(voice) = state.voice_states.get(&conn).filter(seated) {
            // A moderator may have moved the leased seat while we were away.
            return ResumedSeat::Kept(v1::JoinVoice {
                channel_id: voice.channel_id.clone().unwrap_or_default(),
                self_mute: voice.self_mute,
                self_deaf: voice.self_deaf,
            });
        }
        if state.voice_states.values().any(|voice| seated(&voice)) {
            ResumedSeat::OtherDevice
        } else {
            ResumedSeat::Gone
        }
    }
    /// Keep the channel to restore in step with what the server says about
    /// this connection's seat: a moderator move changes it, and a removal
    /// (another device took the seat, a kick, lost access) clears it.
    fn follow_own_voice(&mut self, update: &v1::VoiceStateUpdated) {
        let own = self.store.read().own_conn;
        if own.is_none() || crate::state::id(&update.conn_id).ok() != own {
            return;
        }
        self.desired_voice = update.channel_id.clone().map(|channel_id| v1::JoinVoice {
            channel_id,
            self_mute: update.self_mute,
            self_deaf: update.self_deaf,
        });
    }
    fn accepted(&mut self, message: &Msg) {
        match message {
            Msg::JoinVoice(join) => self.desired_voice = Some(join.clone()),
            Msg::LeaveVoice(_) => self.desired_voice = None,
            Msg::SetVoiceState(state) => {
                if let Some(join) = &mut self.desired_voice {
                    join.self_mute = state.self_mute;
                    join.self_deaf = state.self_deaf;
                }
            }
            Msg::Typing(typing) => {
                self.typing
                    .insert(typing.channel_id.to_vec(), Instant::now());
            }
            _ => {}
        }
    }
}

/// The account's voice seat after a resume, from the new snapshot.
enum ResumedSeat {
    /// This connection still holds it; restore this channel from now on.
    Kept(v1::JoinVoice),
    /// Another device signed in to the account holds it.
    OtherDevice,
    /// No seat: join again.
    Gone,
}

async fn while_reconnecting<T>(
    future: impl std::future::Future<Output = T>,
    commands: &mut mpsc::Receiver<Command>,
    pending: &mut VecDeque<Pending>,
) -> Option<T> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return Some(result),
            command = commands.recv() => {
                let command = command?;
                if !command.replayable {
                    let _ = command.reply.send(Err(Error::Disconnected));
                } else if pending.len() >= MAX_PENDING {
                    let _ = command.reply.send(Err(Error::Control(
                        ss_net::control::ControlError::Capacity,
                    )));
                } else {
                    pending.push_back(Pending { command, sent: None });
                }
            }
        }
    }
}
/// Whether `error` ends reconnecting. Of the server's refusals only the
/// account and version ones are final; a `RATE_LIMITED` resume, or one the
/// server failed to answer, is tried again (`02-protocol.md` §1.1).
fn terminal(error: &Error) -> bool {
    if let Error::Server { code, .. } = error {
        return matches!(
            v1::ErrorCode::try_from(*code),
            Ok(v1::ErrorCode::Unauthenticated
                | v1::ErrorCode::Banned
                | v1::ErrorCode::Forbidden
                | v1::ErrorCode::ProtocolVersion)
        );
    }
    matches!(
        error,
        Error::Protocol(_)
            | Error::Control(
                ss_net::control::ControlError::Protocol(_)
                    | ss_net::control::ControlError::Frame(_)
            )
            | Error::Storage(_)
            | Error::Network(
                ss_net::Error::Untrusted { .. }
                    | ss_net::Error::FingerprintChanged { .. }
                    | ss_net::Error::Config(_)
            )
    )
}
/// The application close code the peer closed with, if it did.
fn close_code(quic: &quinn::Connection) -> Option<u32> {
    match quic.close_reason()? {
        quinn::ConnectionError::ApplicationClosed(close) => {
            u32::try_from(close.error_code.into_inner()).ok()
        }
        _ => None,
    }
}
/// A uniformly random number of milliseconds in `0..=max`.
fn random_millis(max: u64) -> u64 {
    let mut bytes = [0; 8];
    let _ = getrandom::fill(&mut bytes);
    u64::from_le_bytes(bytes) % (max + 1)
}
/// 2, 4, 8, 16, then 24 s, each plus up to a quarter more. The first
/// attempt waits 2–2.5 s and none more than 30 s. The jitter stays at the
/// cap, so clients that lost one server do not come back in lockstep.
fn backoff(attempt: u32) -> Duration {
    let base = (2_000_u64 << attempt.saturating_sub(1).min(4)).min(24_000);
    Duration::from_millis(base + random_millis(base / 4))
}
/// A server-stated wait, plus up to a quarter more so clients refused
/// together spread out. Never shorter than the wait.
fn stated_delay(wait: Duration) -> Duration {
    let wait = wait.min(MAX_STATED_WAIT);
    let millis = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
    wait + Duration::from_millis(random_millis(millis / 4))
}

/// Send one command and await its correlated reply.
async fn call(control: &ClientControl, message: Msg) -> Result<v1::Ack, Error> {
    match timeout(DEADLINE, control.call(message))
        .await
        .map_err(|_| Error::Timeout)??
    {
        Reply::Ack(ack) => Ok(ack),
        Reply::Error(error) => Err(error.into()),
    }
}

async fn reply(inflight: &mut Option<InFlight<'_>>) -> Result<v1::Ack, Error> {
    match inflight {
        Some(inflight) => inflight.reply.as_mut().await,
        None => std::future::pending().await,
    }
}

async fn next_page(resync: &mut Option<Resync>) -> Option<ResyncPage> {
    match resync {
        Some(resync) => resync.pages.recv().await,
        None => std::future::pending().await,
    }
}

/// One history page fetched by a [`Resync`], with the id range it is
/// authoritative for.
struct ResyncPage {
    channel: ss_core::Id,
    span: crate::state::Span,
    messages: Vec<v1::Message>,
}

/// Re-reads the cached messages of every channel once the client is
/// `Connected` again, newest page first. Each page replaces the cached
/// messages in its id range, so edits and deletes made while offline show,
/// and new messages come in with it. A channel stops at the page that
/// reaches its oldest cached message, at its first message, or after
/// [`RESYNC_PAGES`] pages; then anything older is dropped, so the cache
/// never holds a silent gap and older history loads on demand. Pages
/// merge between live events; dropping this (the connection ended) aborts
/// the fetch.
struct Resync {
    generation: u64,
    pages: mpsc::Receiver<ResyncPage>,
    task: JoinHandle<()>,
    /// Live creates and edits (`Some`) and deletes (`None`) seen since the
    /// fetch began. They are at least as new as any page fetched before them.
    live: HashMap<ss_core::Id, Option<v1::Message>>,
}

impl Resync {
    fn start(
        address: &ServerAddr,
        trust: ss_net::pins::TrustPolicy,
        token: &str,
        generation: u64,
        floors: Vec<(ss_core::Id, ss_core::Id)>,
        events: broadcast::Sender<ClientEvent>,
    ) -> Option<Self> {
        if floors.is_empty() {
            return None;
        }
        let history = crate::history::History::new(address, trust, token);
        let (sender, pages) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            let deferred = |error: Error| {
                let _ = events.send(ClientEvent::Notice(format!(
                    "Message resync deferred: {error}"
                )));
            };
            let history = match history {
                Ok(history) => history,
                Err(error) => return deferred(error),
            };
            for (channel, floor) in floors {
                let mut before = None;
                for fetched in 1..=RESYNC_PAGES {
                    let messages = match resync_page(&history, channel, before).await {
                        Ok(page) => page,
                        Err(error) => {
                            deferred(error);
                            break;
                        }
                    };
                    let oldest = messages
                        .first()
                        .and_then(|first| crate::state::id(&first.id).ok());
                    let start = messages.len() < 50;
                    let covered = oldest.is_none_or(|oldest| oldest <= floor);
                    let budget = fetched == RESYNC_PAGES;
                    let span = crate::state::Span {
                        // At the channel's first message, or out of pages:
                        // nothing older stays.
                        from: if start || (budget && !covered) {
                            None
                        } else {
                            oldest
                        },
                        before,
                    };
                    let page = ResyncPage {
                        channel,
                        span,
                        messages,
                    };
                    if sender.send(page).await.is_err() {
                        return;
                    }
                    if start || covered || budget {
                        break;
                    }
                    before = oldest;
                }
            }
        });
        Some(Self {
            generation,
            pages,
            task,
            live: HashMap::new(),
        })
    }

    fn observe(&mut self, event: &v1::ServerEnvelope) {
        let (message, gone) = match &event.msg {
            Some(
                Event::MessageCreated(v1::MessageCreated {
                    message: Some(message),
                    ..
                })
                | Event::MessageUpdated(v1::MessageUpdated {
                    message: Some(message),
                    ..
                }),
            ) => (message, false),
            Some(Event::MessageDeleted(v1::MessageDeleted {
                message: Some(message),
                ..
            })) => (message, true),
            _ => return,
        };
        if let Ok(key) = crate::state::id(&message.id) {
            self.live.insert(key, (!gone).then(|| message.clone()));
        }
    }

    /// Replace page entries that a live edit or delete has already superseded.
    fn reconcile(&self, page: Vec<v1::Message>) -> Vec<v1::Message> {
        page.into_iter()
            .filter_map(|message| {
                match crate::state::id(&message.id)
                    .ok()
                    .and_then(|key| self.live.get(&key))
                {
                    Some(live) => live.clone(),
                    None => Some(message),
                }
            })
            .collect()
    }
}

/// One resync page. A `429` waits as long as the server says, plus up to a
/// quarter more so clients behind one address spread out, as a refused
/// resume does; after [`RESYNC_RATE_LIMITS`] of them the channel is deferred.
async fn resync_page(
    history: &crate::history::History,
    channel: ss_core::Id,
    before: Option<ss_core::Id>,
) -> Result<Vec<v1::Message>, Error> {
    let mut refused = 0;
    loop {
        match history.page(channel, before, None, None).await {
            Err(Error::Server {
                code, retry_after, ..
            }) if code == i32::from(v1::ErrorCode::RateLimited) && refused < RESYNC_RATE_LIMITS => {
                refused += 1;
                let wait = retry_after.unwrap_or(Duration::from_secs(1));
                tokio::time::sleep(stated_delay(wait)).await;
            }
            result => return result,
        }
    }
}

impl Drop for Resync {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const CLOCK_JUMP: Duration = Duration::from_secs(3);

/// One heartbeat reading of both clocks.
#[derive(Debug, Clone, Copy)]
struct ClockSample {
    monotonic: Instant,
    wall: SystemTime,
}

/// Heartbeat clock source. Tests skew the wall clock to simulate a sleep.
#[derive(Clone, Default)]
pub(crate) struct Clock {
    /// Seconds added to the wall clock.
    #[cfg(test)]
    pub(crate) wall_skew: Arc<std::sync::atomic::AtomicU64>,
}

impl Clock {
    #[cfg_attr(not(test), allow(clippy::unused_self))]
    fn sample(&self) -> ClockSample {
        let wall = SystemTime::now();
        #[cfg(test)]
        let wall =
            wall + Duration::from_secs(self.wall_skew.load(std::sync::atomic::Ordering::Relaxed));
        ClockSample {
            monotonic: Instant::now(),
            wall,
        }
    }
}

/// Whether the connection missed time between heartbeats: the loop stalled
/// (`Instant` jumped), or the wall clock moved unlike `Instant`. On macOS
/// `Instant` stops during sleep, so only the wall clock shows a sleep there.
/// A wall-clock step of more than three seconds also reconnects; that is
/// harmless.
fn clock_jumped(last: ClockSample, now: ClockSample) -> bool {
    let monotonic = now.monotonic.saturating_duration_since(last.monotonic);
    let drift = match now.wall.duration_since(last.wall) {
        Ok(wall) => wall.abs_diff(monotonic),
        Err(backwards) => backwards.duration() + monotonic,
    };
    monotonic > CLOCK_JUMP || drift > CLOCK_JUMP
}

/// Time between two heartbeat samples, the larger of the two clocks: on
/// macOS `Instant` stops while the machine sleeps.
fn since(earlier: ClockSample, now: ClockSample) -> Duration {
    let monotonic = now.monotonic.saturating_duration_since(earlier.monotonic);
    now.wall
        .duration_since(earlier.wall)
        .map_or(monotonic, |wall| wall.max(monotonic))
}

fn make_replayable(message: &mut Msg) -> bool {
    let Msg::SendMessage(send) = message else {
        return false;
    };
    if send.nonce.as_ref().is_none_or(String::is_empty) {
        send.nonce = Some(ss_core::Id::generate().to_string());
    }
    true
}

#[cfg(test)]
mod pump_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_notifications_reject_cached_echoes_and_snapshot_overlap() {
        let channel = ss_core::Id::generate();
        let key = ss_core::Id::generate();
        let message = v1::Message {
            id: key.as_bytes().to_vec().into(),
            channel_id: channel.as_bytes().to_vec().into(),
            ..Default::default()
        };
        let store = StateStore::default();
        store
            .apply(v1::server_envelope::Msg::ChannelCreated(
                v1::ChannelCreated {
                    channel: Some(v1::Channel {
                        id: channel.as_bytes().to_vec().into(),
                        effective_permissions: Some(ss_core::Permissions::EVERYONE.bits()),
                        ..Default::default()
                    }),
                },
            ))
            .unwrap();
        assert!(is_new_live_message(&store.read(), &message));
        store
            .apply(v1::server_envelope::Msg::MessageCreated(
                v1::MessageCreated {
                    message: Some(message.clone()),
                    ..Default::default()
                },
            ))
            .unwrap();
        assert!(!is_new_live_message(&store.read(), &message));
        let mut view = crate::State::default();
        view.channels.insert(
            channel,
            v1::Channel {
                last_message_id: Some(message.id.clone()),
                ..Default::default()
            },
        );
        assert!(!is_new_live_message(&view, &message));
        view.channels.clear();
        view.direct_messages.insert(
            channel,
            v1::Channel {
                last_message_id: Some(message.id.clone()),
                ..Default::default()
            },
        );
        assert!(!is_new_live_message(&view, &message));
    }

    #[test]
    fn reconnect_backoff_and_clock_jump_thresholds_are_bounded() {
        assert!(backoff(1) >= Duration::from_secs(2));
        assert!(backoff(1) <= Duration::from_millis(2500));
        assert!(backoff(u32::MAX) <= Duration::from_secs(30));
        let last = ClockSample {
            monotonic: Instant::now(),
            wall: SystemTime::now(),
        };
        let after = |monotonic: Duration, wall: Duration| ClockSample {
            monotonic: last.monotonic + monotonic,
            wall: last.wall + wall,
        };
        let second = Duration::from_secs(1);
        let three = Duration::from_secs(3);
        let over = three + Duration::from_millis(1);
        assert!(!clock_jumped(last, after(three, three)));
        // The loop itself stalled.
        assert!(clock_jumped(last, after(over, over)));
        // macOS sleep: `Instant` saw a second, the wall clock a minute.
        assert!(!clock_jumped(last, after(second, second + three)));
        assert!(clock_jumped(last, after(second, second + over)));
        assert!(clock_jumped(last, after(second, Duration::from_secs(60))));
        // A backward wall-clock step.
        let back = ClockSample {
            monotonic: last.monotonic + second,
            wall: last.wall - Duration::from_secs(3),
        };
        assert!(clock_jumped(last, back));
    }

    #[test]
    fn a_mention_a_resync_page_brought_first_is_counted_and_announced_once() {
        let own = ss_core::Id::generate();
        let channel = ss_core::Id::generate();
        let wire = |id: ss_core::Id| Bytes::from(id.as_bytes().to_vec());
        let store = StateStore::default();
        store
            .welcome(&v1::Welcome {
                user: Some(v1::User {
                    id: wire(own),
                    ..Default::default()
                }),
                snapshot: Some(v1::Snapshot {
                    channels: vec![v1::Channel {
                        id: wire(channel),
                        kind: v1::ChannelKind::Text.into(),
                        effective_permissions: Some(ss_core::Permissions::EVERYONE.bits()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            })
            .unwrap();
        // Written after the snapshot was built, so the snapshot's count
        // does not include it.
        let mention = v1::Message {
            id: wire(ss_core::Id::generate()),
            channel_id: wire(channel),
            author: Some(v1::User {
                id: wire(ss_core::Id::generate()),
                ..Default::default()
            }),
            mentions: vec![wire(own)],
            ..Default::default()
        };
        // The resync page comes from the database over HTTPS and the live
        // event over QUIC: the page can be merged first.
        let generation = store.read().generation;
        resync_page(&store, generation, channel, vec![mention.clone()]);
        let created = || {
            v1::server_envelope::Msg::MessageCreated(v1::MessageCreated {
                message: Some(mention.clone()),
                nonce: None,
            })
        };
        assert!(
            is_new_live_message(&store.read(), &mention),
            "the live mention would not be notified"
        );
        store.apply(created()).unwrap();
        assert_eq!(
            store
                .read()
                .read_states
                .get(&channel)
                .map(|read| read.mention_count),
            Some(1),
            "the live mention was not counted"
        );
        // An echo of the same event is neither counted nor notified again.
        assert!(!is_new_live_message(&store.read(), &mention));
        store.apply(created()).unwrap();
        assert_eq!(store.read().read_states[&channel].mention_count, 1);
    }

    fn resync_page(
        store: &StateStore,
        generation: u64,
        channel: ss_core::Id,
        page: Vec<v1::Message>,
    ) {
        let whole = crate::state::Span {
            from: None,
            before: None,
        };
        store
            .resync(generation, channel, whole, page, |_| false)
            .unwrap();
    }

    #[test]
    fn backoff_keeps_its_jitter_at_the_cap() {
        let capped: std::collections::BTreeSet<_> = (0..64).map(|_| backoff(20)).collect();
        assert!(capped.iter().all(|delay| *delay <= Duration::from_secs(30)));
        assert!(
            capped.len() > 1,
            "every capped delay was {:?}: clients that failed together retry together",
            capped.first()
        );
    }

    #[test]
    fn only_account_and_version_refusals_end_reconnecting() {
        let server = |code: v1::ErrorCode| {
            Error::from(v1::Error {
                code: code.into(),
                retry_after_ms: Some(1500),
                ..Default::default()
            })
        };
        for code in [
            v1::ErrorCode::Unauthenticated,
            v1::ErrorCode::Banned,
            v1::ErrorCode::Forbidden,
            v1::ErrorCode::ProtocolVersion,
        ] {
            assert!(terminal(&server(code)), "{code:?}");
        }
        for code in [
            v1::ErrorCode::RateLimited,
            v1::ErrorCode::Internal,
            v1::ErrorCode::Invalid,
            v1::ErrorCode::Unspecified,
        ] {
            assert!(!terminal(&server(code)), "{code:?}");
        }
    }

    #[tokio::test]
    async fn the_reconnect_queue_is_bounded() {
        let (sender, mut commands) = mpsc::channel(MAX_PENDING + 1);
        let mut pending = VecDeque::new();
        let mut replies = Vec::new();
        for _ in 0..=MAX_PENDING {
            let (reply, response) = oneshot::channel();
            sender
                .try_send(Command {
                    message: Msg::SendMessage(v1::SendMessage::default()),
                    generation: 0,
                    replayable: true,
                    reply,
                })
                .unwrap();
            replies.push(response);
        }
        while_reconnecting(
            tokio::time::sleep(Duration::from_millis(50)),
            &mut commands,
            &mut pending,
        )
        .await
        .unwrap();
        assert_eq!(pending.len(), MAX_PENDING);
        let overflow = replies.pop().unwrap().await.unwrap();
        assert!(
            matches!(
                overflow,
                Err(Error::Control(ss_net::control::ControlError::Capacity))
            ),
            "{overflow:?}"
        );
    }

    #[test]
    fn only_message_sends_are_made_replayable() {
        let mut send = Msg::SendMessage(v1::SendMessage {
            nonce: None,
            ..Default::default()
        });
        assert!(make_replayable(&mut send));
        let Msg::SendMessage(send) = send else {
            unreachable!()
        };
        assert!(send.nonce.is_some_and(|nonce| !nonce.is_empty()));
        let mut typing = Msg::Typing(v1::Typing::default());
        assert!(!make_replayable(&mut typing));
    }
}
