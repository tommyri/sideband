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
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, timeout},
};

pub(crate) const DEADLINE: Duration = Duration::from_secs(10);
const RECONNECT_CLOSE_CODE: u32 = 73;

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
    outgoing: broadcast::Sender<Bytes>,
    incoming: broadcast::Sender<IncomingMedia>,
    stats: Arc<RwLock<TransportStats>>,
    quic: Arc<RwLock<Option<quinn::Connection>>>,
}

impl MediaLink {
    fn new() -> Self {
        let (outgoing, _) = broadcast::channel(256);
        let (incoming, _) = broadcast::channel(512);
        Self {
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
}

/// One independently owned client. Drop aborts its worker, which drops the
/// control router and closes QUIC. No worker retains an Arc back to Client.
pub struct Client {
    pub(crate) commands: mpsc::Sender<Command>,
    pub(crate) store: StateStore,
    pub(crate) events: broadcast::Sender<ClientEvent>,
    pub(crate) auth: Arc<RwLock<Option<String>>>,
    pub(crate) address: ServerAddr,
    pub(crate) pins: Arc<dyn PinStore>,
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) media: MediaLink,
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
        let auth = Arc::new(RwLock::new(Some(welcome.resume_token)));
        let (events, _) = broadcast::channel(256);
        let (commands, command_rx) = mpsc::channel(64);
        let media = MediaLink::new();
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
        };
        let worker = tokio::spawn(runner.run(live, command_rx));
        Ok(Self {
            commands,
            store,
            events,
            auth,
            address,
            pins,
            storage,
            media,
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
}
impl Drop for Live {
    fn drop(&mut self) {
        self.connection.close(0, b"client session ended");
    }
}
impl Live {
    fn control(&self) -> &ClientControl {
        self.control
            .as_ref()
            .expect("establish installs the router before returning Live")
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
    !view.messages.get(&channel).is_some_and(|cache| {
        cache
            .ordered()
            .iter()
            .chain(cache.deleted().iter())
            .any(|cached| cached.id == message.id)
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
    auth: Arc<RwLock<Option<String>>>,
    desired_voice: Option<v1::JoinVoice>,
    typing: HashMap<Vec<u8>, Instant>,
    pending: VecDeque<Command>,
    media: MediaLink,
}
impl Runner {
    fn status(&self, state: ConnectionState) {
        self.store.connection(state.clone());
        let _ = self.events.send(ClientEvent::Connection(state));
    }
    fn event(&self, event: v1::ServerEnvelope) -> Result<bool, Error> {
        let message = event.msg.ok_or(Error::Protocol("empty event"))?;
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
        self.media.connected(Some(&live.connection));
        loop {
            let outcome = self.connected(&mut live, &mut commands).await;
            if outcome.as_ref().is_err_and(|error| !terminal(error)) {
                live.connection
                    .close(RECONNECT_CLOSE_CODE, b"client reconnecting");
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
            let token = self.auth.read().clone();
            let Some(mut token) = token else {
                self.status(ConnectionState::Disconnected);
                return;
            };
            let mut attempt = 0_u32;
            loop {
                attempt = attempt.saturating_add(1);
                let delay = backoff(attempt);
                self.status(ConnectionState::Reconnecting { attempt, delay });
                if while_reconnecting(tokio::time::sleep(delay), &mut commands, &mut self.pending)
                    .await
                    .is_none()
                {
                    return;
                }
                let result = while_reconnecting(
                    establish(
                        &self.address,
                        self.pins.clone(),
                        v1::hello::Auth::ResumeToken(token.clone()),
                    ),
                    &mut commands,
                    &mut self.pending,
                )
                .await;
                let Some(result) = result else {
                    return;
                };
                match result {
                    Ok((mut next, welcome)) => {
                        let setup = async {
                            let cursors = self.store.message_cursors();
                            self.store.install(
                                &welcome,
                                ConnectionState::Reconnecting { attempt, delay },
                            )?;
                            persist(&self.storage, &self.address, &welcome.resume_token).await?;
                            token.clone_from(&welcome.resume_token);
                            *self.auth.write() = Some(welcome.resume_token.clone());
                            self.restore_voice(&mut next).await?;
                            self.resync_messages(&welcome.resume_token, cursors).await;
                            self.flush_pending(&mut next).await?;
                            Ok::<_, Error>(())
                        }
                        .await;
                        if let Err(error) = setup {
                            drop(next);
                            if terminal(&error) {
                                self.status(ConnectionState::Failed(error.to_string()));
                                return;
                            }
                            continue;
                        }
                        self.typing.clear();
                        let _ = self.events.send(ClientEvent::Resynced);
                        self.status(ConnectionState::Connected);
                        self.media.connected(Some(&next.connection));
                        live = next;
                        break;
                    }
                    Err(error) if terminal(&error) => {
                        self.status(ConnectionState::Failed(error.to_string()));
                        return;
                    }
                    Err(_) => {}
                }
            }
        }
    }
    async fn connected(
        &mut self,
        live: &mut Live,
        commands: &mut mpsc::Receiver<Command>,
    ) -> Result<(), Error> {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_tick = Instant::now();
        let mut outgoing = self.media.outgoing.subscribe();
        let mut stats = tokio::time::interval(Duration::from_millis(500));
        stats.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    let now = Instant::now();
                    if clock_jumped(last_tick, now) {
                        return Err(Error::Disconnected);
                    }
                    last_tick = now;
                }
                event = live.incoming.recv() => {
                    let event = event.ok_or(Error::Disconnected)?;
                    if self.event(event)? { return Ok(()); }
                }
                command = commands.recv() => {
                    let Some(command) = command else { return Ok(()); };
                    if command.reply.is_closed() { continue; }
                    if command.generation != self.store.read().generation && !command.replayable {
                        let _ = command.reply.send(Err(Error::Disconnected));
                        continue;
                    }
                    let message = command.message.clone();
                    if let Msg::Typing(typing) = &message
                        && self.typing.get(typing.channel_id.as_ref()).is_some_and(|last| last.elapsed() < Duration::from_secs(5)) {
                            let _ = command.reply.send(Ok(v1::Ack { request_id: 0, result: Some(v1::ack::Result::Empty(v1::Empty {})) }));
                            continue;
                    }
                    let result = self.execute(live, message.clone()).await;
                    if result.is_ok() { self.accepted(&message); }
                    let stop = result.as_ref().err().is_some_and(|error| !matches!(error, Error::Server { .. }) && terminal(error));
                    let retry = result.as_ref().err().is_some_and(|e| !matches!(e, Error::Server { .. }));
                    if retry && command.replayable {
                        self.pending.push_back(command);
                        return Err(Error::Disconnected);
                    }
                    let _ = command.reply.send(result);
                    if stop { return Err(Error::Protocol("command ended the session")); }
                    if retry { return Err(Error::Disconnected); }
                }
                packet = live.connection.read_datagram() => {
                    let packet = packet.map_err(|_| Error::Disconnected)?;
                    let _ = self.media.incoming.send(IncomingMedia {
                        bytes: packet,
                        at: std::time::Instant::now(),
                    });
                }
                packet = outgoing.recv() => {
                    match packet {
                        Ok(packet) => { let _ = live.connection.send_datagram(packet); }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                }
                _ = stats.tick() => {
                    let current = live.connection.stats();
                    *self.media.stats.write() = TransportStats {
                        rtt: current.rtt,
                        lost_packets: current.lost_packets,
                    };
                }
            }
        }
    }
    async fn execute(&self, live: &mut Live, message: Msg) -> Result<v1::Ack, Error> {
        // Drain events while awaiting the Ack; large mutation fan-outs cannot
        // fill the event queue just because a command is outstanding.
        let control = live.control.as_ref().ok_or(Error::Disconnected)?;
        let response = timeout(DEADLINE, control.call(message));
        tokio::pin!(response);
        loop {
            tokio::select! {
                result = &mut response => return match result.map_err(|_| Error::Timeout)?? {
                    Reply::Ack(ack) => Ok(ack), Reply::Error(error) => Err(error.into()),
                },
                event = live.incoming.recv() => {
                    if self.event(event.ok_or(Error::Disconnected)?)? {
                        return Err(Error::Protocol("session removed by server"));
                    }
                }
            }
        }
    }
    async fn flush_pending(&mut self, live: &mut Live) -> Result<(), Error> {
        while let Some(command) = self.pending.pop_front() {
            if command.reply.is_closed() {
                continue;
            }
            let result = self.execute(live, command.message.clone()).await;
            if result
                .as_ref()
                .err()
                .is_some_and(|error| !matches!(error, Error::Server { .. }))
            {
                self.pending.push_front(command);
                return Err(Error::Disconnected);
            }
            if result.is_ok() {
                self.accepted(&command.message);
            }
            let _ = command.reply.send(result);
        }
        Ok(())
    }
    async fn resync_messages(&self, token: &str, cursors: Vec<(ss_core::Id, ss_core::Id)>) {
        for (channel, mut cursor) in cursors {
            for _ in 0..20 {
                let page = match crate::history::fetch_page(
                    &self.address,
                    self.pins.clone(),
                    token,
                    channel,
                    None,
                    Some(cursor),
                    None,
                )
                .await
                {
                    Ok(page) => page,
                    Err(error) => {
                        let _ = self.events.send(ClientEvent::Notice(format!(
                            "Message resync deferred: {error}"
                        )));
                        break;
                    }
                };
                let page_len = page.len();
                let Some(last) = page.last() else { break };
                let Ok(next) = crate::state::id(&last.id) else {
                    break;
                };
                if self.store.resync(channel, page).is_err() {
                    break;
                }
                let _ = self.events.send(ClientEvent::MessagesChanged { channel });
                cursor = next;
                if page_len < 50 {
                    break;
                }
            }
        }
    }
    async fn restore_voice(&mut self, live: &mut Live) -> Result<(), Error> {
        let Some(join) = self.desired_voice.clone() else {
            return Ok(());
        };
        let already_joined = {
            let state = self.store.read();
            let own_user = state.own_user.as_ref().map(|user| &user.id);
            state.voice_states.values().any(|voice| {
                Some(&voice.user_id) == own_user
                    && voice.channel_id.as_ref() == Some(&join.channel_id)
            })
        };
        let result = if already_joined {
            Ok(v1::Ack::default())
        } else {
            self.execute(live, Msg::JoinVoice(join)).await
        };
        match result {
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

async fn while_reconnecting<T>(
    future: impl std::future::Future<Output = T>,
    commands: &mut mpsc::Receiver<Command>,
    pending: &mut VecDeque<Command>,
) -> Option<T> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return Some(result),
            command = commands.recv() => {
                let command = command?;
                if command.replayable {
                    pending.push_back(command);
                } else {
                    let _ = command.reply.send(Err(Error::Disconnected));
                }
            }
        }
    }
}
fn terminal(error: &Error) -> bool {
    matches!(
        error,
        Error::Protocol(_)
            | Error::Control(
                ss_net::control::ControlError::Protocol(_)
                    | ss_net::control::ControlError::Frame(_)
            )
            | Error::Storage(_)
            | Error::Server { .. }
            | Error::Network(
                ss_net::Error::Untrusted { .. }
                    | ss_net::Error::FingerprintChanged { .. }
                    | ss_net::Error::Config(_)
            )
    )
}
fn backoff(attempt: u32) -> Duration {
    let base = (2_u64 << attempt.saturating_sub(1).min(4)).min(30) * 1000;
    let mut bytes = [0; 2];
    let _ = getrandom::getrandom(&mut bytes);
    // Equal jitter, bounded by 30 seconds. First attempt waits 2–2.5 s.
    let jitter = u64::from(u16::from_le_bytes(bytes)) % (base / 4 + 1);
    Duration::from_millis((base + jitter).min(30_000))
}

fn clock_jumped(last: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last) > Duration::from_secs(3)
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
        let now = Instant::now();
        assert!(!clock_jumped(now, now + Duration::from_secs(3)));
        assert!(clock_jumped(
            now,
            now + Duration::from_secs(3) + Duration::from_millis(1)
        ));
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
