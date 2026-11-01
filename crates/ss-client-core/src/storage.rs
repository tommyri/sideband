//! Host-owned persistence. Secrets never appear in Debug or client events.

use crate::{Error, ServerAddr};
use parking_lot::Mutex;
use ss_core::Id;
use ss_net::pins::ServerId;
use std::collections::BTreeMap;

/// Persist sessions and the last server. Calls run on blocking workers, so a
/// host can use a file or OS keychain. Implementations must serialize writes.
/// Certificate persistence uses the separate `ss_net::pins::PinStore` trait.
pub trait Storage: Send + Sync + 'static {
    /// Previously saved session, if any.
    fn session(&self, server: &ServerId) -> Result<Option<String>, Error>;
    /// Save/replace the token from Welcome (also valid for HTTP bearer auth).
    fn save_session(&self, server: &ServerId, token: &str) -> Result<(), Error>;
    /// Most recently connected server.
    fn last_server(&self) -> Result<Option<ServerAddr>, Error>;
    /// Save the last successfully authenticated server.
    fn save_last_server(&self, server: &ServerAddr) -> Result<(), Error>;
    /// Saved linear playback gain for one remote user.
    fn user_volume(&self, _server: &ServerId, _user: Id) -> Result<Option<f32>, Error> {
        Ok(None)
    }
    /// Save linear playback gain for one remote user.
    fn save_user_volume(&self, _server: &ServerId, _user: Id, _gain: f32) -> Result<(), Error> {
        Ok(())
    }
    /// Whether one remote user is muted only on this client.
    fn user_muted(&self, _server: &ServerId, _user: Id) -> Result<bool, Error> {
        Ok(false)
    }
    /// Save this client's local mute for one remote user.
    fn save_user_muted(&self, _server: &ServerId, _user: Id, _muted: bool) -> Result<(), Error> {
        Ok(())
    }
}

/// Ephemeral storage for tests or callers that do not want saved sessions.
#[derive(Default)]
pub struct MemoryStorage {
    sessions: Mutex<BTreeMap<ServerId, String>>,
    last: Mutex<Option<ServerAddr>>,
    volumes: Mutex<BTreeMap<(ServerId, Id), f32>>,
    muted: Mutex<BTreeMap<(ServerId, Id), bool>>,
}

impl Storage for MemoryStorage {
    fn session(&self, server: &ServerId) -> Result<Option<String>, Error> {
        Ok(self.sessions.lock().get(server).cloned())
    }
    fn save_session(&self, server: &ServerId, token: &str) -> Result<(), Error> {
        self.sessions
            .lock()
            .insert(server.clone(), token.to_owned());
        Ok(())
    }
    fn last_server(&self) -> Result<Option<ServerAddr>, Error> {
        Ok(self.last.lock().clone())
    }
    fn save_last_server(&self, server: &ServerAddr) -> Result<(), Error> {
        *self.last.lock() = Some(server.clone());
        Ok(())
    }
    fn user_volume(&self, server: &ServerId, user: Id) -> Result<Option<f32>, Error> {
        Ok(self.volumes.lock().get(&(server.clone(), user)).copied())
    }
    fn save_user_volume(&self, server: &ServerId, user: Id, gain: f32) -> Result<(), Error> {
        self.volumes.lock().insert((server.clone(), user), gain);
        Ok(())
    }
    fn user_muted(&self, server: &ServerId, user: Id) -> Result<bool, Error> {
        Ok(self
            .muted
            .lock()
            .get(&(server.clone(), user))
            .copied()
            .unwrap_or(false))
    }
    fn save_user_muted(&self, server: &ServerId, user: Id, muted: bool) -> Result<(), Error> {
        self.muted.lock().insert((server.clone(), user), muted);
        Ok(())
    }
}
