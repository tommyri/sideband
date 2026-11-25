use crate::persistence::DesktopStorage;
use ss_client_core::{Client, Error as ClientError};
use ss_net::pins::{JsonPinStore, ServerId};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::Mutex;

pub struct AppState {
    pub transfers: Mutex<std::collections::HashMap<String, tokio::sync::watch::Sender<bool>>>,
    /// Synchronous on purpose: its guard cannot be held across an `.await`
    /// in a command (the future would not be `Send`, and clippy's
    /// `await_holding_lock` rejects it elsewhere). Clone a client out with
    /// [`AppState::client`] before awaiting the network.
    clients: std::sync::Mutex<Clients>,
    pub storage: Arc<DesktopStorage>,
    pub profile_path: std::path::PathBuf,
    pub relay_selection: Mutex<()>,
    pub pins: Arc<JsonPinStore>,
    pub operations: Mutex<()>,
    pub voice: Mutex<Option<crate::voice::ActiveVoice>>,
    pub audio_test: Mutex<Option<Arc<AtomicBool>>>,
    pub close_to_tray: AtomicBool,
}

impl AppState {
    pub fn open(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            transfers: Mutex::new(std::collections::HashMap::new()),
            clients: std::sync::Mutex::new(Clients::default()),
            storage: Arc::new(DesktopStorage::open(path.join("sessions.json"))?),
            profile_path: path.join("profile.json"),
            relay_selection: Mutex::new(()),
            pins: Arc::new(JsonPinStore::open(path.join("pins.json"))?),
            operations: Mutex::new(()),
            voice: Mutex::new(None),
            audio_test: Mutex::new(None),
            close_to_tray: AtomicBool::new(true),
        })
    }

    /// The client map, for synchronous reads and ownership changes only.
    pub fn clients(&self) -> std::sync::MutexGuard<'_, Clients> {
        lock(&self.clients)
    }

    /// A shared handle to one server's client. The map lock is released before
    /// this returns, so a stalled server cannot block commands for another.
    pub fn client(&self, server: &ServerId) -> Option<Arc<Client>> {
        self.clients().get(server).cloned()
    }
}

/// Lock a std mutex even if a panic poisoned it. The shell's std mutexes guard
/// registries and caches that each update changes with one assignment or
/// insert, so the value stays usable, and one panic must not make every later
/// command panic.
pub fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Connected clients by normalized server identity. Commands share a client
/// through an `Arc` while they await it; removal closes it with [`close`].
#[derive(Default)]
pub struct Clients(BTreeMap<ServerId, Arc<Client>>);

impl Clients {
    /// Add a connected client. Returns the previous one, which the caller
    /// should [`close`].
    pub fn insert(&mut self, client: Client) -> Result<Option<Arc<Client>>, ClientError> {
        Ok(self.0.insert(client.address().id()?, Arc::new(client)))
    }
    pub fn get(&self, server: &ServerId) -> Option<&Arc<Client>> {
        self.0.get(server)
    }
    pub fn remove(&mut self, server: &ServerId) -> Option<Arc<Client>> {
        self.0.remove(server)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&ServerId, &Arc<Client>)> {
        self.0.iter()
    }
}

/// Stop a removed client now, even while a command still holds a handle to
/// it: its pending commands fail as disconnected and it cannot reconnect
/// (or rejoin voice) later. Waits for the worker when this is the last handle.
pub async fn close(client: Arc<Client>) {
    client.abort();
    if let Ok(client) = Arc::try_unwrap(client) {
        client.close().await;
    }
}
