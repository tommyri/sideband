//! Connection ownership, authentication, bounded commands, and session resume.

use crate::{
    ClientEvent, ConnectionState, Error, StateStore,
    storage::{MemoryStorage, Storage},
};
use parking_lot::RwLock;
use ss_net::{
    control::{ClientControl, Reply},
    pins::{PinStore, ServerId},
};
use ss_proto::v1::{self, client_envelope::Msg, server_envelope::Msg as Event};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, timeout},
};

pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

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
    pub reply: oneshot::Sender<Result<v1::Ack, Error>>,
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
        let runner = Runner {
            address: address.clone(),
            pins: pins.clone(),
            storage,
            store: store.clone(),
            events: events.clone(),
            auth: auth.clone(),
            desired_voice: None,
            typing: HashMap::new(),
        };
        let worker = tokio::spawn(runner.run(live, command_rx));
        Ok(Self {
            commands,
            store,
            events,
            auth,
            address,
            pins,
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
    /// manager; application commands use correlated Ack/Error replies. Commands
    /// are never replayed after reconnect because their outcome may be unknown.
    pub async fn command(&self, message: Msg) -> Result<v1::Ack, Error> {
        if matches!(message, Msg::Hello(_) | Msg::Ping(_)) {
            return Err(Error::Protocol("connection-owned command"));
        }
        let generation = {
            let state = self.store.read();
            if state.connection != ConnectionState::Connected {
                return Err(Error::Disconnected);
            }
            state.generation
        };
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command {
                message,
                generation,
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
                client_name: "SocialSpeak".into(),
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

struct Runner {
    address: ServerAddr,
    pins: Arc<dyn PinStore>,
    storage: Arc<dyn Storage>,
    store: StateStore,
    events: broadcast::Sender<ClientEvent>,
    auth: Arc<RwLock<Option<String>>>,
    desired_voice: Option<v1::JoinVoice>,
    typing: HashMap<Vec<u8>, Instant>,
}
impl Runner {
    fn status(&self, state: ConnectionState) {
        self.store.connection(state.clone());
        let _ = self.events.send(ClientEvent::Connection(state));
    }
    fn event(&self, event: v1::ServerEnvelope) -> Result<bool, Error> {
        let message = event.msg.ok_or(Error::Protocol("empty event"))?;
        let event = self.store.apply(message)?;
        let removed = matches!(event, ClientEvent::Removed { .. });
        let _ = self.events.send(event);
        Ok(removed)
    }
    async fn run(mut self, mut live: Live, mut commands: mpsc::Receiver<Command>) {
        loop {
            let outcome = self.connected(&mut live, &mut commands).await;
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
                if while_rejecting(tokio::time::sleep(delay), &mut commands)
                    .await
                    .is_none()
                {
                    return;
                }
                let result = while_rejecting(
                    establish(
                        &self.address,
                        self.pins.clone(),
                        v1::hello::Auth::ResumeToken(token.clone()),
                    ),
                    &mut commands,
                )
                .await;
                let Some(result) = result else {
                    return;
                };
                match result {
                    Ok((mut next, welcome)) => {
                        let setup = async {
                            self.store.install(
                                &welcome,
                                ConnectionState::Reconnecting { attempt, delay },
                            )?;
                            persist(&self.storage, &self.address, &welcome.resume_token).await?;
                            token.clone_from(&welcome.resume_token);
                            *self.auth.write() = Some(welcome.resume_token);
                            if let Some(join) = self.desired_voice.clone() {
                                match self.execute(&mut next, Msg::JoinVoice(join)).await {
                                    Ok(_) => {}
                                    Err(Error::Server { message, .. }) => {
                                        self.desired_voice = None;
                                        let _ = self.events.send(ClientEvent::Notice(format!(
                                            "Could not rejoin voice: {message}"
                                        )));
                                    }
                                    Err(error) => return Err(error),
                                }
                            }
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
        loop {
            tokio::select! {
                event = live.incoming.recv() => {
                    let event = event.ok_or(Error::Disconnected)?;
                    if self.event(event)? { return Ok(()); }
                }
                command = commands.recv() => {
                    let Some(command) = command else { return Ok(()); };
                    if command.reply.is_closed() { continue; }
                    if command.generation != self.store.read().generation {
                        let _ = command.reply.send(Err(Error::Disconnected));
                        continue;
                    }
                    let message = command.message;
                    if let Msg::Typing(typing) = &message
                        && self.typing.get(typing.channel_id.as_ref()).is_some_and(|last| last.elapsed() < Duration::from_secs(5)) {
                            let _ = command.reply.send(Ok(v1::Ack { request_id: 0, result: Some(v1::ack::Result::Empty(v1::Empty {})) }));
                            continue;
                    }
                    let result = self.execute(live, message.clone()).await;
                    if result.is_ok() { self.accepted(&message); }
                    let stop = result.as_ref().err().is_some_and(|error| !matches!(error, Error::Server { .. }) && terminal(error));
                    let retry = result.as_ref().err().is_some_and(|e| !matches!(e, Error::Server { .. }));
                    let _ = command.reply.send(result);
                    if stop { return Err(Error::Protocol("command ended the session")); }
                    if retry { return Err(Error::Disconnected); }
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

async fn while_rejecting<T>(
    future: impl std::future::Future<Output = T>,
    commands: &mut mpsc::Receiver<Command>,
) -> Option<T> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return Some(result),
            command = commands.recv() => { let command = command?; let _ = command.reply.send(Err(Error::Disconnected)); }
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
    let base = (1_u64 << attempt.saturating_sub(1).min(5)).min(30) * 1000;
    let mut bytes = [0; 2];
    let _ = getrandom::getrandom(&mut bytes);
    // Equal jitter, bounded by 30 seconds. First attempt waits 1–1.25 s.
    let jitter = u64::from(u16::from_le_bytes(bytes)) % (base / 4 + 1);
    Duration::from_millis((base + jitter).min(30_000))
}
