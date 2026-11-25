//! Desktop-owned persistence. Tokens never cross the webview bridge.
use crate::state::lock;
use serde::{Deserialize, Serialize};
use ss_client_core::{Error, ServerAddr, storage::Storage};
use ss_net::pins::ServerId;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedServer {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub https_port: u16,
    pub label: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Data {
    servers: Vec<SavedServer>,
    sessions: BTreeMap<String, String>,
    last: Option<String>,
    #[serde(default)]
    volumes: BTreeMap<String, f32>,
    #[serde(default)]
    notifications: ss_client_core::notifications::Preferences,
    #[serde(default)]
    muted: BTreeMap<String, bool>,
}

/// Reads copy from memory and never wait for the disk. Writes are serialized
/// by `writer`, which is held while the file is written and synced; `data` is
/// held only to copy or replace the in-memory value. Write methods block, so
/// async callers run them on a blocking worker ([`DesktopStorage::write`]).
pub struct DesktopStorage {
    path: PathBuf,
    data: Mutex<Data>,
    writer: Mutex<()>,
}

impl DesktopStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let data = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| Error::Storage(e.to_string()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(e) => return Err(Error::Storage(e.to_string())),
        };
        Ok(Self {
            path,
            data: Mutex::new(data),
            writer: Mutex::new(()),
        })
    }

    /// Run a blocking storage write from async code without stalling a
    /// runtime thread on the file sync.
    pub async fn write<T: Send + 'static>(
        self: &Arc<Self>,
        change: impl FnOnce(&Self) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let storage = Arc::clone(self);
        tauri::async_runtime::spawn_blocking(move || change(&storage))
            .await
            .map_err(|e| Error::Storage(e.to_string()))?
    }

    fn data(&self) -> std::sync::MutexGuard<'_, Data> {
        lock(&self.data)
    }

    fn update(&self, change: impl FnOnce(&mut Data)) -> Result<(), Error> {
        let _writer = lock(&self.writer);
        let mut next = self.data().clone();
        change(&mut next);
        let write = || -> Result<(), Box<dyn std::error::Error>> {
            let parent = self
                .path
                .parent()
                .ok_or("the storage path has no parent directory")?;
            std::fs::create_dir_all(parent)?;
            // NamedTempFile is owner-only on Unix; Windows inherits the user's
            // application-data directory ACL. Never store passwords here.
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer(file.as_file_mut(), &next)?;
            file.as_file().sync_all()?;
            file.persist(&self.path)?;
            Ok(())
        };
        write().map_err(|e| Error::Storage(e.to_string()))?;
        *self.data() = next;
        Ok(())
    }

    pub fn notifications(&self) -> ss_client_core::notifications::Preferences {
        self.data().notifications.clone()
    }
    pub fn save_notifications(
        &self,
        preferences: ss_client_core::notifications::Preferences,
    ) -> Result<(), Error> {
        self.update(|data| data.notifications = preferences)
    }

    pub fn servers(&self) -> Vec<SavedServer> {
        self.data().servers.clone()
    }

    pub fn label(&self, id: &str, label: String) -> Result<(), Error> {
        self.update(|data| {
            if let Some(server) = data.servers.iter_mut().find(|s| s.id == id) {
                server.label = label;
            }
        })
    }

    pub fn remove(&self, id: &str) -> Result<(), Error> {
        self.update(|data| {
            data.servers.retain(|s| s.id != id);
            data.sessions.remove(id);
            if data.last.as_deref() == Some(id) {
                data.last = None;
            }
        })
    }

    pub fn reorder(&self, ids: &[String], existing: Vec<SavedServer>) -> Result<(), Error> {
        let unique: std::collections::BTreeSet<_> = ids.iter().collect();
        if ids.len() != existing.len()
            || unique.len() != ids.len()
            || existing.iter().any(|s| !unique.contains(&s.id))
        {
            return Err(Error::Storage(
                "Server order must contain each saved server once.".into(),
            ));
        }
        self.update(|data| {
            for saved in existing {
                if !data.servers.iter().any(|s| s.id == saved.id) {
                    data.servers.push(saved);
                }
            }
            data.servers
                .sort_by_key(|s| ids.iter().position(|id| id == &s.id));
        })
    }
}

impl Storage for DesktopStorage {
    fn user_volume(&self, server: &ServerId, user: ss_core::Id) -> Result<Option<f32>, Error> {
        Ok(self
            .data()
            .volumes
            .get(&format!("{}/{user}", server.as_str()))
            .copied())
    }
    fn save_user_volume(
        &self,
        server: &ServerId,
        user: ss_core::Id,
        gain: f32,
    ) -> Result<(), Error> {
        self.update(|data| {
            data.volumes
                .insert(format!("{}/{user}", server.as_str()), gain);
        })
    }
    fn user_muted(&self, server: &ServerId, user: ss_core::Id) -> Result<bool, Error> {
        Ok(self
            .data()
            .muted
            .get(&format!("{}/{user}", server.as_str()))
            .copied()
            .unwrap_or(false))
    }
    fn save_user_muted(
        &self,
        server: &ServerId,
        user: ss_core::Id,
        muted: bool,
    ) -> Result<(), Error> {
        self.update(|data| {
            data.muted
                .insert(format!("{}/{user}", server.as_str()), muted);
        })
    }
    fn session(&self, server: &ServerId) -> Result<Option<String>, Error> {
        Ok(self.data().sessions.get(server.as_str()).cloned())
    }
    fn save_session(&self, server: &ServerId, token: &str) -> Result<(), Error> {
        self.update(|data| {
            data.sessions
                .insert(server.as_str().to_owned(), token.to_owned());
        })
    }
    fn last_server(&self) -> Result<Option<ServerAddr>, Error> {
        let data = self.data();
        Ok(data
            .servers
            .iter()
            .find(|s| Some(&s.id) == data.last.as_ref())
            .map(|s| ServerAddr {
                host: s.host.clone(),
                port: s.port,
                https_port: s.https_port,
            }))
    }
    fn save_last_server(&self, server: &ServerAddr) -> Result<(), Error> {
        let id = server.id()?.as_str().to_owned();
        self.update(|data| {
            if let Some(saved) = data.servers.iter_mut().find(|s| s.id == id) {
                saved.https_port = server.https_port;
            } else {
                data.servers.push(SavedServer {
                    id: id.clone(),
                    host: server.host.clone(),
                    port: server.port,
                    https_port: server.https_port,
                    label: server.host.clone(),
                });
            }
            data.last = Some(id);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_panic_during_a_write_leaves_storage_usable_and_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let storage = DesktopStorage::open(dir.path().join("sessions.json")).unwrap();
        let server = ServerAddr::new("localhost", 5781).unwrap();
        storage.save_last_server(&server).unwrap();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            storage.update(|data| {
                data.servers.clear();
                panic!("change failed");
            })
        }));
        assert!(panicked.is_err());
        assert_eq!(storage.servers().len(), 1);
        storage
            .label(server.id().unwrap().as_str(), "Home".into())
            .unwrap();
        assert_eq!(storage.servers()[0].label, "Home");
    }
    /// A `sessions.json` written before notification policy moved to
    /// `ss-client-core` still loads, and saving writes the same shape back.
    #[test]
    fn saved_notification_preferences_keep_their_shape() {
        let sample = serde_json::json!({
            "servers": [{
                "id": "localhost:5781",
                "host": "localhost",
                "port": 5781,
                "https_port": 5782,
                "label": "Home"
            }],
            "sessions": {},
            "last": "localhost:5781",
            "volumes": {},
            "notifications": {
                "desktop": true,
                "previews": false,
                "dnd": true,
                "mutedServers": ["localhost:5781"],
                "channels": {
                    "localhost:5781/01HZX5V4Q4M7K8Y9Z0A1B2C3D4": "all",
                    "localhost:5781/01HZX5V4Q4M7K8Y9Z0A1B2C3D5": "mentions",
                    "localhost:5781/01HZX5V4Q4M7K8Y9Z0A1B2C3D6": "none"
                }
            },
            "muted": {}
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        std::fs::write(&path, sample.to_string()).unwrap();
        let storage = DesktopStorage::open(&path).unwrap();
        let prefs = storage.notifications();
        assert!(prefs.desktop && !prefs.previews && prefs.dnd);
        assert!(prefs.muted_servers.contains("localhost:5781"));
        assert_eq!(prefs.channels.len(), 3);
        storage.save_notifications(prefs).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["notifications"], sample["notifications"]);
        // Files from before notification preferences existed still load.
        std::fs::write(&path, r#"{"servers":[],"sessions":{},"last":null}"#).unwrap();
        let old = DesktopStorage::open(&path).unwrap().notifications();
        assert!(!old.desktop && !old.dnd && old.channels.is_empty());
    }
    #[test]
    fn reads_do_not_wait_for_a_write_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let storage = DesktopStorage::open(dir.path().join("sessions.json")).unwrap();
        let _writing = lock(&storage.writer);
        assert!(storage.servers().is_empty());
        assert!(storage.last_server().unwrap().is_none());
    }
    #[test]
    fn sessions_and_order_survive_restart_and_removal_forgets_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        let storage = DesktopStorage::open(&path).unwrap();
        let a = ServerAddr::new("localhost", 5781).unwrap();
        let b = ServerAddr::new("example.org", 5781).unwrap();
        storage
            .save_session(&a.id().unwrap(), "secret-token")
            .unwrap();
        storage.save_last_server(&a).unwrap();
        storage.save_last_server(&b).unwrap();
        storage
            .reorder(
                &[
                    b.id().unwrap().as_str().into(),
                    a.id().unwrap().as_str().into(),
                ],
                storage.servers(),
            )
            .unwrap();
        let reopened = DesktopStorage::open(&path).unwrap();
        assert_eq!(reopened.servers()[0].host, "example.org");
        assert_eq!(
            reopened.session(&a.id().unwrap()).unwrap().as_deref(),
            Some("secret-token")
        );
        reopened.remove(a.id().unwrap().as_str()).unwrap();
        assert!(
            DesktopStorage::open(&path)
                .unwrap()
                .session(&a.id().unwrap())
                .unwrap()
                .is_none()
        );
        assert!(reopened.reorder(&[], reopened.servers()).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
}
