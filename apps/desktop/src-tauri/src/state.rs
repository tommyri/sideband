use ss_client_core::{ClientSet, storage::MemoryStorage};
use ss_net::pins::MemoryPinStore;
use std::sync::{Arc, atomic::AtomicBool};
use tokio::sync::Mutex;

pub struct AppState {
    pub clients: Mutex<ClientSet>,
    pub storage: Arc<MemoryStorage>,
    pub pins: Arc<MemoryPinStore>,
    pub close_to_tray: AtomicBool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            clients: Mutex::new(ClientSet::default()),
            storage: Arc::new(MemoryStorage::default()),
            pins: Arc::new(MemoryPinStore::default()),
            close_to_tray: AtomicBool::new(true),
        }
    }
}
