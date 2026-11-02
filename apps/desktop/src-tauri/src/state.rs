use crate::persistence::DesktopStorage;
use ss_client_core::ClientSet;
use ss_net::pins::JsonPinStore;
use std::sync::{Arc, atomic::AtomicBool};
use tokio::sync::Mutex;

pub struct AppState {
    pub clients: Mutex<ClientSet>,
    pub storage: Arc<DesktopStorage>,
    pub pins: Arc<JsonPinStore>,
    pub operations: Mutex<()>,
    pub close_to_tray: AtomicBool,
}

impl AppState {
    pub fn open(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            clients: Mutex::new(ClientSet::default()),
            storage: Arc::new(DesktopStorage::open(path.join("sessions.json"))?),
            pins: Arc::new(JsonPinStore::open(path.join("pins.json"))?),
            operations: Mutex::new(()),
            close_to_tray: AtomicBool::new(true),
        })
    }
}
