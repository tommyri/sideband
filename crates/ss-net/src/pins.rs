//! Pin storage keyed by the original host and port, never the resolved address.
//!
//! File operations are synchronous; `client::connect` runs them on the blocking
//! pool. Applications should do the same when loading or approving pins.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use parking_lot::Mutex;

use crate::{Error, tls::Fingerprint};

/// Canonical pin key. DNS names are case-insensitive; IPv6 brackets are removed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServerId(String);

impl ServerId {
    /// Builds an unambiguous host/port key; IP addresses are normalized.
    pub fn new(host: &str, port: u16) -> Result<Self, Error> {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|_| Error::Config("invalid server name"))?;
        let normalized = match name {
            rustls::pki_types::ServerName::IpAddress(ip) => std::net::IpAddr::from(ip).to_string(),
            rustls::pki_types::ServerName::DnsName(dns) => {
                dns.as_ref().trim_end_matches('.').to_ascii_lowercase()
            }
            _ => return Err(Error::Config("unsupported server name")),
        };
        Ok(Self(format!("[{normalized}]:{port}")))
    }

    /// Stable key used by JSON storage.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Shared pin store. Implementations must serialize `remember` with updates.
pub trait PinStore: Send + Sync + 'static {
    /// Reads an accepted fingerprint.
    fn get(&self, server: &ServerId) -> Result<Option<Fingerprint>, Error>;
    /// Explicit user approval, including replacement of an old pin.
    fn set(&self, server: &ServerId, fingerprint: Fingerprint) -> Result<(), Error>;
    /// Records a publicly verified certificate, refusing to overwrite a different pin.
    fn remember(&self, server: &ServerId, fingerprint: Fingerprint) -> Result<(), Error>;
}

fn check_existing(
    pins: &BTreeMap<String, Fingerprint>,
    server: &ServerId,
    received: Fingerprint,
) -> Result<(), Error> {
    if let Some(&expected) = pins.get(server.as_str())
        && expected != received
    {
        return Err(Error::FingerprintChanged { expected, received });
    }
    Ok(())
}

/// Process-local store, useful for the CLI and tests.
#[derive(Debug, Default)]
pub struct MemoryPinStore(Mutex<BTreeMap<String, Fingerprint>>);

impl PinStore for MemoryPinStore {
    fn get(&self, server: &ServerId) -> Result<Option<Fingerprint>, Error> {
        Ok(self.0.lock().get(server.as_str()).copied())
    }
    fn set(&self, server: &ServerId, fingerprint: Fingerprint) -> Result<(), Error> {
        self.0.lock().insert(server.0.clone(), fingerprint);
        Ok(())
    }
    fn remember(&self, server: &ServerId, fingerprint: Fingerprint) -> Result<(), Error> {
        let mut pins = self.0.lock();
        check_existing(&pins, server, fingerprint)?;
        pins.insert(server.0.clone(), fingerprint);
        Ok(())
    }
}

/// JSON file with atomic replacement. Share one instance per file via `Arc`.
/// Separate processes must not write the same pin file concurrently.
#[derive(Debug)]
pub struct JsonPinStore {
    path: PathBuf,
    pins: Mutex<BTreeMap<String, Fingerprint>>,
}

impl JsonPinStore {
    /// Loads existing pins. Missing files start empty; corrupt files are errors.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let pins = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            path,
            pins: Mutex::new(pins),
        })
    }

    fn update(
        &self,
        server: &ServerId,
        fingerprint: Fingerprint,
        replace: bool,
    ) -> Result<(), Error> {
        let mut pins = self.pins.lock();
        if !replace {
            check_existing(&pins, server, fingerprint)?;
            if pins.contains_key(server.as_str()) {
                return Ok(());
            }
        }
        let mut next = pins.clone();
        next.insert(server.0.clone(), fingerprint);
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(temp.as_file_mut(), &next)?;
        temp.as_file().sync_all()?;
        temp.persist(&self.path).map_err(|error| error.error)?;
        *pins = next;
        Ok(())
    }
}

impl PinStore for JsonPinStore {
    fn get(&self, server: &ServerId) -> Result<Option<Fingerprint>, Error> {
        Ok(self.pins.lock().get(server.as_str()).copied())
    }
    fn set(&self, server: &ServerId, fingerprint: Fingerprint) -> Result<(), Error> {
        self.update(server, fingerprint, true)
    }
    fn remember(&self, server: &ServerId, fingerprint: Fingerprint) -> Result<(), Error> {
        self.update(server, fingerprint, false)
    }
}
