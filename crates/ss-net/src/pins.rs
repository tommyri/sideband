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

/// Persisted identity policy. Public trust never falls back to a fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
pub enum TrustPolicy {
    PublicCertificate,
    PinnedCertificate { fingerprint: Fingerprint },
}

/// Shared trust store; changes and compare-and-set operations are serialized.
pub trait PinStore: Send + Sync + 'static {
    fn policy(&self, server: &ServerId) -> Result<Option<TrustPolicy>, Error>;
    fn compare_exchange(
        &self,
        server: &ServerId,
        expected: Option<TrustPolicy>,
        next: TrustPolicy,
    ) -> Result<(), Error>;
    fn get(&self, server: &ServerId) -> Result<Option<Fingerprint>, Error> {
        Ok(match self.policy(server)? {
            Some(TrustPolicy::PinnedCertificate { fingerprint }) => Some(fingerprint),
            _ => None,
        })
    }
    /// Explicit approval. Public trust cannot be downgraded through this API.
    fn set(&self, server: &ServerId, fingerprint: Fingerprint) -> Result<(), Error> {
        let previous = self.policy(server)?;
        if previous == Some(TrustPolicy::PublicCertificate) {
            return Err(Error::Config(
                "public certificate trust cannot be downgraded to a pin",
            ));
        }
        self.compare_exchange(
            server,
            previous,
            TrustPolicy::PinnedCertificate { fingerprint },
        )
    }
    /// Record a pin only if it does not replace an existing trust decision.
    fn remember(&self, server: &ServerId, fingerprint: Fingerprint) -> Result<(), Error> {
        if let Some(expected) = self.get(server)? {
            if expected == fingerprint {
                return Ok(());
            }
            return Err(Error::FingerprintChanged {
                expected,
                received: fingerprint,
            });
        }
        self.compare_exchange(server, None, TrustPolicy::PinnedCertificate { fingerprint })
    }
}

fn exchange(
    policies: &mut BTreeMap<String, TrustPolicy>,
    server: &ServerId,
    expected: Option<TrustPolicy>,
    next: TrustPolicy,
) -> Result<(), Error> {
    if policies.get(server.as_str()).copied() != expected {
        return Err(Error::Config(
            "server trust changed during verification; retry",
        ));
    }
    policies.insert(server.0.clone(), next);
    Ok(())
}

#[derive(Debug, Default)]
pub struct MemoryPinStore(Mutex<BTreeMap<String, TrustPolicy>>);
impl PinStore for MemoryPinStore {
    fn policy(&self, server: &ServerId) -> Result<Option<TrustPolicy>, Error> {
        Ok(self.0.lock().get(server.as_str()).copied())
    }
    fn compare_exchange(
        &self,
        server: &ServerId,
        expected: Option<TrustPolicy>,
        next: TrustPolicy,
    ) -> Result<(), Error> {
        exchange(&mut self.0.lock(), server, expected, next)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    servers: BTreeMap<String, TrustPolicy>,
}

/// Versioned JSON with atomic replacement. Legacy fingerprints remain strict pins.
/// Share one instance per file; separate processes must not write concurrently.
#[derive(Debug)]
pub struct JsonPinStore {
    path: PathBuf,
    policies: Mutex<BTreeMap<String, TrustPolicy>>,
}
impl JsonPinStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let (policies, migrate) = match std::fs::read(&path) {
            Ok(bytes) => {
                let value: serde_json::Value = serde_json::from_slice(&bytes)?;
                if value.get("version").is_some() {
                    let doc: Document = serde_json::from_value(value)?;
                    if doc.version != 2 {
                        return Err(Error::Config("unsupported trust store version"));
                    }
                    (doc.servers, false)
                } else {
                    let legacy: BTreeMap<String, Fingerprint> = serde_json::from_value(value)?;
                    (
                        legacy
                            .into_iter()
                            .map(|(key, fingerprint)| {
                                (key, TrustPolicy::PinnedCertificate { fingerprint })
                            })
                            .collect(),
                        true,
                    )
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (BTreeMap::new(), false),
            Err(error) => return Err(error.into()),
        };
        let store = Self {
            path,
            policies: Mutex::new(policies),
        };
        if migrate {
            store.persist(&store.policies.lock())?;
        }
        Ok(store)
    }
    fn persist(&self, policies: &BTreeMap<String, TrustPolicy>) -> Result<(), Error> {
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(
            temp.as_file_mut(),
            &Document {
                version: 2,
                servers: policies.clone(),
            },
        )?;
        temp.as_file().sync_all()?;
        temp.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}
impl PinStore for JsonPinStore {
    fn policy(&self, server: &ServerId) -> Result<Option<TrustPolicy>, Error> {
        Ok(self.policies.lock().get(server.as_str()).copied())
    }
    fn compare_exchange(
        &self,
        server: &ServerId,
        expected: Option<TrustPolicy>,
        next: TrustPolicy,
    ) -> Result<(), Error> {
        let mut current = self.policies.lock();
        let mut updated = current.clone();
        exchange(&mut updated, server, expected, next)?;
        self.persist(&updated)?;
        *current = updated;
        Ok(())
    }
}
