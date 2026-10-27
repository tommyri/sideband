//! Persistent server identities and certificate fingerprints. TLS stays on 0.23.

use std::{fmt, path::Path, sync::Arc};

use arc_swap::ArcSwap;
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ALPN, Error};

/// SHA-256 of the exact leaf certificate DER, displayed as lowercase hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Fingerprint([u8; 32]);

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl From<Fingerprint> for String {
    fn from(value: Fingerprint) -> Self {
        value.to_string()
    }
}

impl TryFrom<String> for Fingerprint {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64 || !value.is_ascii() {
            return Err("fingerprint must be 64 hex digits");
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| "invalid fingerprint hex")?;
        }
        Ok(Self(bytes))
    }
}

/// Computes a fingerprint without parsing or re-encoding the certificate.
pub fn fingerprint(der: &[u8]) -> Fingerprint {
    Fingerprint(Sha256::digest(der).into())
}

/// Leaf-first chain and matching private key. No secret material in `Debug`.
pub struct TlsConfig {
    /// DER certificate chain, leaf first.
    pub certificates: Vec<CertificateDer<'static>>,
    /// Private key matching the leaf certificate.
    pub private_key: PrivateKeyDer<'static>,
}

impl TlsConfig {
    pub(crate) fn server_config(self) -> Result<quinn::ServerConfig, Error> {
        let resolver = Arc::new(CertResolver::new(self)?);
        server_config(resolver)
    }
}

/// Leaf certificate swapped in place. The QUIC socket keeps this resolver.
#[derive(Debug)]
pub struct CertResolver {
    current: ArcSwap<CertifiedKey>,
}

impl CertResolver {
    /// Installs the leaf. An empty chain is rejected.
    pub fn new(tls: TlsConfig) -> Result<Self, Error> {
        Ok(Self {
            current: ArcSwap::new(Arc::new(certified(tls)?)),
        })
    }

    /// Replaces the certificate future handshakes present. Open connections stay up.
    pub fn replace(&self, tls: TlsConfig) -> Result<Fingerprint, Error> {
        let fingerprint = leaf_fingerprint(&tls)?;
        self.current.store(Arc::new(certified(tls)?));
        Ok(fingerprint)
    }

    /// SHA-256 of the leaf currently offered to new handshakes.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let current = self.current.load();
        let der = current
            .cert
            .first()
            .map(std::convert::AsRef::as_ref)
            .unwrap_or_default();
        fingerprint(der)
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current.load_full())
    }
}

fn certified(tls: TlsConfig) -> Result<CertifiedKey, Error> {
    if tls.certificates.is_empty() {
        return Err(Error::Config("certificate chain is empty"));
    }
    let provider = rustls::crypto::ring::default_provider();
    CertifiedKey::from_der(tls.certificates, tls.private_key, &provider).map_err(Error::from)
}

fn leaf_fingerprint(tls: &TlsConfig) -> Result<Fingerprint, Error> {
    let leaf = tls
        .certificates
        .first()
        .ok_or(Error::Config("certificate chain is empty"))?;
    Ok(fingerprint(leaf.as_ref()))
}

pub(crate) fn server_config(
    resolver: Arc<dyn ResolvesServerCert>,
) -> Result<quinn::ServerConfig, Error> {
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_no_client_auth()
    .with_cert_resolver(resolver);
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .map_err(|_| Error::Config("TLS provider lacks QUIC cipher"))?;
    Ok(quinn::ServerConfig::with_crypto(Arc::new(crypto)))
}

/// Reads `tls/identity.json` without generating a replacement.
pub fn read_identity(path: &Path) -> Result<TlsConfig, Error> {
    identity_from_bytes(&std::fs::read(path)?)
}

fn identity_from_bytes(bytes: &[u8]) -> Result<TlsConfig, Error> {
    let identity: Identity = serde_json::from_slice(bytes)?;
    Ok(TlsConfig {
        certificates: vec![CertificateDer::from(identity.certificate)],
        private_key: PrivatePkcs8KeyDer::from(identity.key_pkcs8).into(),
    })
}

#[derive(Serialize, Deserialize)]
struct Identity {
    certificate: Vec<u8>,
    key_pkcs8: Vec<u8>,
}

/// Loads or generates `data_dir/tls/identity.json` on the blocking pool.
/// The cert/key pair is installed atomically, with owner-only file permissions
/// on Unix. Corrupt identities fail instead of silently rotating the pin.
pub async fn generate_self_signed(data_dir: impl AsRef<Path>) -> Result<TlsConfig, Error> {
    let directory = data_dir.as_ref().join("tls");
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("identity.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
                let identity = Identity {
                    certificate: key.cert.der().to_vec(),
                    key_pkcs8: key.signing_key.serialize_der(),
                };
                let mut temp = tempfile::NamedTempFile::new_in(&directory)?;
                serde_json::to_writer(temp.as_file_mut(), &identity)?;
                temp.as_file().sync_all()?;
                match temp.persist_noclobber(&path) {
                    Ok(_) => {}
                    Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(Error::Io(error.error)),
                }
                std::fs::read(&path)?
            }
            Err(error) => return Err(error.into()),
        };
        let config = identity_from_bytes(&bytes)?;
        // Validate persisted pairs before returning them, without changing the file.
        let _ = TlsConfig {
            certificates: config.certificates.clone(),
            private_key: config.private_key.clone_key(),
        }
        .server_config()?;
        Ok(config)
    })
    .await?
}
