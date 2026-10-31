//! QUIC client with TOFU pins and ordinary WebPKI trust for public certificates.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

use parking_lot::Mutex;
use rustls::{
    client::{
        WebPkiServerVerifier,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
};

use crate::{
    ALPN, Connection, Error,
    pins::{PinStore, ServerId},
    tls::{Fingerprint, fingerprint},
};

#[derive(Debug, Clone, Copy)]
enum Rejection {
    Untrusted(Fingerprint),
    Changed(Fingerprint, Fingerprint),
}

#[derive(Debug)]
struct Verifier {
    pin: Option<Fingerprint>,
    rejection: Arc<Mutex<Option<Rejection>>>,
    observed: Arc<Mutex<Option<Fingerprint>>>,
    webpki: Arc<WebPkiServerVerifier>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let received = fingerprint(end_entity.as_ref());
        *self.observed.lock() = Some(received);
        let rejection = match self.pin {
            Some(expected) if expected == received => return Ok(ServerCertVerified::assertion()),
            Some(expected) => Rejection::Changed(expected, received),
            None => {
                if let Ok(verified) = self.webpki.verify_server_cert(
                    end_entity,
                    intermediates,
                    server_name,
                    ocsp,
                    now,
                ) {
                    return Ok(verified);
                }
                Rejection::Untrusted(received)
            }
        };
        *self.rejection.lock() = Some(rejection);
        Err(rustls::Error::General(
            "certificate pin approval required".to_owned(),
        ))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// TLS configuration for HTTPS using an already approved QUIC leaf pin.
/// Uses the same verifier and handshake signature checks as QUIC. The caller
/// must obtain this pin from its store; this function never approves a peer.
pub fn pinned_tls_config(pin: Fingerprint) -> Result<rustls::ClientConfig, Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|_| Error::Config("invalid CA roots"))?;
    Ok(
        rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Verifier {
                pin: Some(pin),
                rejection: Arc::default(),
                observed: Arc::default(),
                webpki,
                provider,
            }))
            .with_no_client_auth(),
    )
}

/// Connects to a hostname or bare IP. No application data is sent before trust
/// succeeds. Unknown self-signed certificates return `Untrusted`; pin changes
/// return `FingerprintChanged`. Explicit approval is `PinStore::set`, then retry.
/// Publicly trusted certificates are accepted by WebPKI and recorded as pins.
pub async fn connect(host: &str, port: u16, pins: Arc<dyn PinStore>) -> Result<Connection, Error> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    connect_with_roots(host, port, pins, roots).await
}

/// Connects with caller-supplied CA roots (e.g. an organization's private CA).
/// Pins still take precedence over CA verification, including on replacement.
pub async fn connect_with_roots(
    host: &str,
    port: u16,
    pins: Arc<dyn PinStore>,
    roots: rustls::RootCertStore,
) -> Result<Connection, Error> {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.');
    let addresses: Vec<_> = tokio::net::lookup_host((host, port)).await?.collect();
    connect_resolved(host, port, pins, roots, addresses).await
}

async fn connect_resolved(
    host: &str,
    port: u16,
    pins: Arc<dyn PinStore>,
    roots: rustls::RootCertStore,
    addresses: Vec<SocketAddr>,
) -> Result<Connection, Error> {
    let server = ServerId::new(host, port)?;
    let pin = {
        let store = pins.clone();
        let server = server.clone();
        tokio::task::spawn_blocking(move || store.get(&server)).await??
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|_| Error::Config("invalid or empty CA root store"))?;
    let rejection = Arc::new(Mutex::new(None));
    let observed = Arc::new(Mutex::new(None));
    let verifier = Arc::new(Verifier {
        pin,
        rejection: rejection.clone(),
        observed: observed.clone(),
        webpki,
        provider: provider.clone(),
    });
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|_| Error::Config("TLS provider lacks QUIC cipher"))?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(crate::transport_config(true)));
    let mut last_error = Error::Config("hostname resolved to no addresses");
    for address in addresses {
        let unspecified = if address.is_ipv4() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        };
        *rejection.lock() = None;
        *observed.lock() = None;
        // Socket-family and synchronous connect errors belong to this address,
        // just like handshake errors. Another DNS answer may still work.
        let result = async {
            let mut endpoint = crate::bind_endpoint(SocketAddr::new(unspecified, 0), None)?;
            endpoint.set_default_client_config(config.clone());
            let connection = endpoint.connect(address, host)?.await?;
            Ok::<_, Error>((connection, endpoint))
        }
        .await;
        if let Some(rejected) = *rejection.lock() {
            return Err(match rejected {
                Rejection::Untrusted(fingerprint) => Error::Untrusted { fingerprint },
                Rejection::Changed(expected, received) => {
                    Error::FingerprintChanged { expected, received }
                }
            });
        }
        match result {
            Ok((inner, endpoint)) => {
                let connection = Connection::new(inner, endpoint, true)?;
                let fingerprint = observed
                    .lock()
                    .ok_or(Error::Config("TLS peer omitted certificate"))?;
                let store = pins.clone();
                let server = server.clone();
                let remembered =
                    tokio::task::spawn_blocking(move || store.remember(&server, fingerprint))
                        .await?;
                if let Err(error) = remembered {
                    connection.close(1, b"pin persistence failed");
                    return Err(error);
                }
                return Ok(connection);
            }
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pins::MemoryPinStore, server::Endpoint, tls::generate_self_signed};

    #[tokio::test]
    async fn address_setup_failure_tries_next_address() {
        let directory = tempfile::tempdir().unwrap();
        let tls = generate_self_signed(directory.path()).await.unwrap();
        let expected = fingerprint(tls.certificates[0].as_ref());
        let server = Endpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), tls).unwrap();
        let address = server.local_addr().unwrap();
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(
            &ServerId::new("localhost", address.port()).unwrap(),
            expected,
        )
        .unwrap();
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let client = connect_resolved(
            "localhost",
            address.port(),
            pins,
            roots,
            vec![(Ipv4Addr::UNSPECIFIED, 0).into(), address],
        );
        let exchange = async {
            let accept = async { server.accept().await.unwrap().accept().await };
            let (client, accepted) = tokio::join!(client, accept);
            let client = client.unwrap();
            let peer = accepted.unwrap();
            client
                .send_datagram(bytes::Bytes::from_static(b"fallback connected"))
                .unwrap();
            assert_eq!(
                peer.read_datagram().await.unwrap().as_ref(),
                b"fallback connected"
            );
            client.close(0, b"test finished");
        };
        tokio::time::timeout(std::time::Duration::from_secs(3), exchange)
            .await
            .unwrap();
    }
}
