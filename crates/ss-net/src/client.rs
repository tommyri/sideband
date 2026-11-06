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
    pins::{PinStore, ServerId, TrustPolicy},
    tls::{Fingerprint, fingerprint},
};

#[derive(Debug, Clone, Copy)]
enum Rejection {
    Untrusted(Fingerprint),
    Changed(Fingerprint, Fingerprint),
}

#[derive(Debug)]
struct Verifier {
    policy: Option<TrustPolicy>,
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
        let rejection = match self.policy {
            Some(TrustPolicy::PinnedCertificate {
                fingerprint: expected,
            }) if expected == received => return Ok(ServerCertVerified::assertion()),
            Some(TrustPolicy::PinnedCertificate {
                fingerprint: expected,
            }) => Rejection::Changed(expected, received),
            policy => {
                match self.webpki.verify_server_cert(
                    end_entity,
                    intermediates,
                    server_name,
                    ocsp,
                    now,
                ) {
                    Ok(verified) => return Ok(verified),
                    Err(error) => {
                        // Only first-use self-signed identities are eligible for TOFU.
                        // A configured public endpoint never offers a downgrade prompt.
                        if policy.is_some() || !self_signed(end_entity.as_ref(), now) {
                            return Err(error);
                        }
                    }
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
    tls_config(TrustPolicy::PinnedCertificate { fingerprint: pin })
}

/// HTTPS verification using the policy authenticated by the QUIC connection.
pub fn tls_config(policy: TrustPolicy) -> Result<rustls::ClientConfig, Error> {
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
                policy: Some(policy),
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
/// Publicly trusted certificates establish a public-certificate policy, not a leaf pin.
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
    let policy = {
        let store = pins.clone();
        let server = server.clone();
        tokio::task::spawn_blocking(move || store.policy(&server)).await??
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|_| Error::Config("invalid or empty CA root store"))?;
    let rejection = Arc::new(Mutex::new(None));
    let observed = Arc::new(Mutex::new(None));
    let verifier = Arc::new(Verifier {
        policy,
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
                let mut connection = Connection::new(inner, endpoint, true)?;
                let fingerprint = observed
                    .lock()
                    .ok_or(Error::Config("TLS peer omitted certificate"))?;
                let store = pins.clone();
                let server = server.clone();
                let remembered = tokio::task::spawn_blocking(move || {
                    store.compare_exchange(
                        &server,
                        policy,
                        policy.unwrap_or(TrustPolicy::PublicCertificate),
                    )
                })
                .await?;
                if let Err(error) = remembered {
                    connection.close(1, b"pin persistence failed");
                    return Err(error);
                }
                connection.trust = Some(policy.unwrap_or(TrustPolicy::PublicCertificate));
                connection.peer_fingerprint = Some(fingerprint);
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

// Parsing is only used to decide eligibility for an explicit first-use prompt.
// Trust still requires approval and TLS proof of private-key possession.
fn self_signed(der: &[u8], now: UnixTime) -> bool {
    use x509_parser::prelude::*;
    let Ok((rest, cert)) = X509Certificate::from_der(der) else {
        return false;
    };
    let Ok(seconds) = i64::try_from(now.as_secs()) else {
        return false;
    };
    let Ok(time) = ASN1Time::from_timestamp(seconds) else {
        return false;
    };
    rest.is_empty()
        && cert.issuer() == cert.subject()
        && cert.validity().is_valid_at(time)
        && cert.verify_signature(None).is_ok()
}

#[cfg(test)]
mod trust_tests {
    use super::*;

    fn certificate(host: &str) -> CertificateDer<'static> {
        rcgen::generate_simple_self_signed(vec![host.to_owned()])
            .unwrap()
            .cert
            .der()
            .clone()
    }
    fn verifier(policy: Option<TrustPolicy>, roots: &[CertificateDer<'static>]) -> Verifier {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut store = rustls::RootCertStore::empty();
        for root in roots {
            store.add(root.clone()).unwrap();
        }
        Verifier {
            policy,
            rejection: Arc::default(),
            observed: Arc::default(),
            webpki: WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider.clone())
                .build()
                .unwrap(),
            provider,
        }
    }
    fn verify(v: &Verifier, cert: &CertificateDer<'_>) -> bool {
        v.verify_server_cert(
            cert,
            &[],
            &ServerName::try_from("friends.example").unwrap(),
            &[],
            UnixTime::now(),
        )
        .is_ok()
    }
    #[test]
    fn public_renewal_and_distinct_https_leaf_work_without_downgrade() {
        let first = certificate("friends.example");
        let renewed = certificate("friends.example");
        let wrong_host = certificate("elsewhere.example");
        let unknown = certificate("friends.example");
        let public = verifier(
            Some(TrustPolicy::PublicCertificate),
            &[first.clone(), renewed.clone(), wrong_host.clone()],
        );
        assert!(verify(&public, &first));
        assert!(verify(&public, &renewed));
        assert!(!verify(&public, &wrong_host));
        assert!(!verify(&public, &unknown));
        assert!(
            public.rejection.lock().is_none(),
            "public validation errors must not offer pin approval"
        );
        let pinned = verifier(
            Some(TrustPolicy::PinnedCertificate {
                fingerprint: fingerprint(first.as_ref()),
            }),
            std::slice::from_ref(&renewed),
        );
        assert!(verify(&pinned, &first));
        assert!(
            !verify(&pinned, &renewed),
            "CA validity cannot replace explicit trust"
        );
        let first_use = verifier(None, &[first]);
        assert!(!verify(&first_use, &unknown));
        assert!(matches!(
            *first_use.rejection.lock(),
            Some(Rejection::Untrusted(_))
        ));
    }
    #[test]
    fn expired_certificate_is_not_a_first_use_pin_prompt() {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["friends.example".into()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        params.not_after = rcgen::date_time_ymd(2001, 1, 1);
        let expired = params.self_signed(&key).unwrap().der().clone();
        let first_use = verifier(None, &[certificate("friends.example")]);
        assert!(!verify(&first_use, &expired));
        assert!(first_use.rejection.lock().is_none());
        let public = verifier(
            Some(TrustPolicy::PublicCertificate),
            std::slice::from_ref(&expired),
        );
        assert!(!verify(&public, &expired));
    }
}
