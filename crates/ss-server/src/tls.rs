//! Certificate modes for the authenticated listener.
//!
//! `self-signed` reuses `tls/identity.json`. `files` reloads PEM when the
//! files change. `acme` orders a certificate for `public_host` and serves
//! that same leaf on QUIC. Reloads swap the rustls resolver. The UDP socket
//! stays bound.
//!
//! The ACME cache (`tls/acme`, which holds the certificate key) is owner-only.
//! `tls-alpn-01` is answered on TCP at the QUIC port. When `bind_http` already
//! listens there (the `init` default), the HTTPS listener also offers
//! `acme-tls/1`; otherwise a separate listener does, with the public
//! listener's per-address cap and handshake deadline. Either way a validation
//! connection closes after its handshake.

use std::{
    future::Future,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use axum::{
    Router,
    body::Body,
    extract::{Path as AxumPath, State},
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use base64::Engine;
use notify::Watcher;
use rustls::{
    pki_types::{
        CertificateDer, PrivateKeyDer,
        pem::{self, PemObject},
    },
    server::ResolvesServerCert,
};
use rustls_acme::{AcmeConfig, UseChallenge, caches::DirCache};
use sha2::{Digest, Sha256};
use ss_net::tls::{CertResolver, Fingerprint, TlsConfig};
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::Semaphore};
use tokio_rustls::TlsAcceptor;

use crate::{
    config::{self, AcmeChallenge, Config, TlsMode},
    http::hardening::{ACCEPT_BACKOFF, MAX_CONNECTIONS, ServeLimits},
    limits::PeerSessions,
};

/// ALPN protocol of a TLS-ALPN-01 validation handshake (RFC 8737).
pub(crate) const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

type FingerprintSlot = Arc<ArcSwap<Option<Fingerprint>>>;

/// Live certificate. Drop stops the file watcher.
pub struct Identity {
    quic: Arc<dyn ResolvesServerCert>,
    fingerprint: FingerprintSlot,
    challenge: Router,
    redirect: Option<(SocketAddr, Router)>,
    alpn_on_https: bool,
    stop: Arc<AtomicBool>,
}

struct Loaded {
    quic: Arc<dyn ResolvesServerCert>,
    fingerprint: FingerprintSlot,
    challenge: Router,
    redirect: Option<(SocketAddr, Router)>,
    /// The HTTPS listener answers TLS-ALPN-01 (it shares the QUIC port).
    alpn_on_https: bool,
}

impl Identity {
    /// Resolver installed in the QUIC endpoint.
    pub fn resolver(&self) -> Arc<dyn ResolvesServerCert> {
        Arc::clone(&self.quic)
    }

    /// Rustls configuration for the public HTTPS listener. The resolver is
    /// shared with QUIC, so certificate reloads affect new handshakes on both
    /// transports without rebinding either socket. With `tls-alpn-01` on the
    /// HTTPS port it also offers `acme-tls/1`, which the shared ACME resolver
    /// answers with the challenge certificate.
    pub fn https_config(&self) -> Arc<rustls::ServerConfig> {
        let config = https_config(self.resolver());
        if !self.alpn_on_https {
            return config;
        }
        let mut config = Arc::unwrap_or_clone(config);
        config.alpn_protocols.push(ACME_TLS_ALPN.to_vec());
        Arc::new(config)
    }

    /// Leaf offered to the next handshake.
    pub fn fingerprint(&self) -> Option<Fingerprint> {
        **self.fingerprint.load()
    }

    /// Hex fingerprint, or empty before ACME has issued a certificate.
    pub fn fingerprint_text(&self) -> String {
        self.fingerprint()
            .map(|fingerprint| fingerprint.to_string())
            .unwrap_or_default()
    }

    /// Shared with `GET /api/v1/server`, updated on reload.
    pub fn fingerprint_slot(&self) -> FingerprintSlot {
        Arc::clone(&self.fingerprint)
    }

    /// HTTP-01 routes merged onto the public listener. Empty for other modes.
    pub fn challenge_routes(&self) -> Router {
        self.challenge.clone()
    }

    /// Extra TCP 80 listener: the HTTP-01 challenge, then a redirect.
    pub fn redirect(&self) -> Option<(SocketAddr, Router)> {
        self.redirect.clone()
    }
}

/// Builds the public-listener TLS configuration around a live certificate
/// resolver. Transport tests use this to exercise reloads without starting a
/// filesystem watcher.
pub fn https_config(resolver: Arc<dyn ResolvesServerCert>) -> Arc<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(config)
}

impl Drop for Identity {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Loads the configured certificate and starts reload or issuance.
pub async fn prepare(config: &Config, data_dir: &Path) -> Result<Identity, Error> {
    let stop = Arc::new(AtomicBool::new(false));
    let loaded = match config.tls.mode {
        TlsMode::SelfSigned => self_signed(data_dir, &stop).await?,
        TlsMode::Files => files(config, data_dir, &stop)?,
        TlsMode::Acme => acme(config, data_dir, &stop).await?,
    };
    Ok(Identity {
        quic: loaded.quic,
        fingerprint: loaded.fingerprint,
        challenge: loaded.challenge,
        redirect: loaded.redirect,
        alpn_on_https: loaded.alpn_on_https,
        stop,
    })
}

/// Prints the fingerprint `fingerprint` and `run` already show, without listening.
pub async fn current_fingerprint(config: &Config, data_dir: &Path) -> Result<String, Error> {
    match config.tls.mode {
        TlsMode::SelfSigned => self_signed_fingerprint(data_dir).await,
        TlsMode::Files => {
            let tls = read_files(config, data_dir)?;
            leaf_text(&tls)
        }
        TlsMode::Acme => cached_fingerprint(&acme_cert_file(config, data_dir))
            .map(|fingerprint| fingerprint.to_string())
            .ok_or(Error::Invalid("acme has not issued a certificate")),
    }
}

/// Creates or reuses the self-signed identity and returns its fingerprint.
pub async fn self_signed_fingerprint(data_dir: &Path) -> Result<String, Error> {
    leaf_text(&ss_net::tls::generate_self_signed(data_dir).await?)
}

/// Deletes `tls/identity.json` and generates a new self-signed leaf.
pub async fn rotate_self_signed(data_dir: &Path) -> Result<String, Error> {
    let path = data_dir.join("tls").join("identity.json");
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    self_signed_fingerprint(data_dir).await
}

async fn self_signed(data_dir: &Path, stop: &Arc<AtomicBool>) -> Result<Loaded, Error> {
    let tls = ss_net::tls::generate_self_signed(data_dir).await?;
    let resolver = Arc::new(CertResolver::new(tls)?);
    let fingerprint = slot_with(Some(resolver.fingerprint()));
    let path = data_dir.join("tls").join("identity.json");
    let watched = Arc::clone(&resolver);
    let slot = Arc::clone(&fingerprint);
    let targets = watch_targets(std::slice::from_ref(&path));
    spawn_watch(targets, Arc::clone(stop), move || {
        reload_identity(&path, &watched, &slot);
    })?;
    Ok(loaded(resolver, fingerprint))
}

fn files(config: &Config, data_dir: &Path, stop: &Arc<AtomicBool>) -> Result<Loaded, Error> {
    let tls = read_files(config, data_dir)?;
    let resolver = Arc::new(CertResolver::new(tls)?);
    let fingerprint = slot_with(Some(resolver.fingerprint()));
    let cert = located(data_dir, &config.tls.cert_path);
    let key = located(data_dir, &config.tls.key_path);
    let watched = Arc::clone(&resolver);
    let slot = Arc::clone(&fingerprint);
    spawn_watch(
        watch_targets(&[cert.clone(), key.clone()]),
        Arc::clone(stop),
        move || match load_pem(&cert, &key) {
            Ok(tls) => publish(watched.replace(tls), &slot),
            Err(error) => tracing::error!(%error, "certificate files unchanged"),
        },
    )?;
    Ok(loaded(resolver, fingerprint))
}

fn read_files(config: &Config, data_dir: &Path) -> Result<TlsConfig, Error> {
    if config.tls.cert_path.is_empty() || config.tls.key_path.is_empty() {
        return Err(Error::Invalid("files mode requires cert_path and key_path"));
    }
    let cert = located(data_dir, &config.tls.cert_path);
    let key = located(data_dir, &config.tls.key_path);
    if !cert.is_file() || !key.is_file() {
        return Err(Error::Invalid("cert_path and key_path must exist"));
    }
    load_pem(&cert, &key)
}

fn loaded(resolver: Arc<CertResolver>, fingerprint: FingerprintSlot) -> Loaded {
    Loaded {
        quic: resolver,
        fingerprint,
        challenge: Router::new(),
        redirect: None,
        alpn_on_https: false,
    }
}

fn reload_identity(path: &Path, resolver: &CertResolver, slot: &FingerprintSlot) {
    match ss_net::tls::read_identity(path) {
        Ok(tls) => publish(resolver.replace(tls), slot),
        Err(error) => tracing::debug!(%error, "self-signed identity not readable"),
    }
}

fn publish(result: Result<Fingerprint, ss_net::Error>, slot: &FingerprintSlot) {
    match result {
        Ok(fingerprint) => {
            slot.store(Arc::new(Some(fingerprint)));
            tracing::info!(%fingerprint, "certificate reloaded");
        }
        Err(error) => tracing::error!(%error, "certificate rejected"),
    }
}

async fn acme(config: &Config, data_dir: &Path, stop: &Arc<AtomicBool>) -> Result<Loaded, Error> {
    validate_acme(config)?;
    let cache = acme_cache(data_dir);
    private_dir(&cache)?;
    let cert_file = acme_cert_file(config, data_dir);
    let built = acme_config(config, cache);
    let fingerprint = slot_with(cached_fingerprint(&cert_file));
    let state = built.state();
    let resolver = state.resolver();
    drive_acme(
        state,
        cert_file.clone(),
        Arc::clone(&fingerprint),
        Arc::clone(stop),
    );
    let loaded = match config.tls.challenge {
        AcmeChallenge::Http01 => http01_routes(&resolver, config, &fingerprint)?,
        AcmeChallenge::TlsAlpn01
            if shares_tcp_port(config.server.bind_quic, config.server.bind_http) =>
        {
            Loaded {
                quic: resolver,
                fingerprint: Arc::clone(&fingerprint),
                challenge: Router::new(),
                redirect: None,
                alpn_on_https: true,
            }
        }
        AcmeChallenge::TlsAlpn01 => {
            let listener = TcpListener::bind(config.server.bind_quic).await?;
            let stopped = Arc::clone(stop);
            tokio::spawn(serve_challenges(
                listener,
                challenge_config(Arc::clone(&resolver) as Arc<dyn ResolvesServerCert>),
                ServeLimits::default(),
                async move {
                    while !stopped.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                },
            ));
            Loaded {
                quic: resolver,
                fingerprint: Arc::clone(&fingerprint),
                challenge: Router::new(),
                redirect: None,
                alpn_on_https: false,
            }
        }
    };
    if cached_fingerprint(&cert_file).is_none() {
        tracing::info!("acme has not issued a certificate yet");
    }
    let slot = Arc::clone(&loaded.fingerprint);
    let stopped = Arc::clone(stop);
    tokio::spawn(async move {
        while !stopped.load(Ordering::Relaxed) {
            remember_cached(&cert_file, &slot);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    Ok(loaded)
}

/// Whether a TCP listener on `quic` would take the port `http` listens on:
/// the same port, and the same address or a wildcard on either side.
fn shares_tcp_port(quic: SocketAddr, http: SocketAddr) -> bool {
    quic.port() == http.port()
        && (quic.ip() == http.ip() || quic.ip().is_unspecified() || http.ip().is_unspecified())
}

/// Creates `dir` and any missing parent owner-only, and narrows `dir` if it
/// already existed.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        builder.mode(0o700);
        builder.create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    builder.create(dir)?;
    Ok(())
}

fn validate_acme(config: &Config) -> Result<(), Error> {
    let email = config.tls.acme_email.trim();
    if email.is_empty() {
        return Err(Error::Invalid("acme_email is required"));
    }
    if !email_ok(email) {
        return Err(Error::Invalid("acme_email must be an email address"));
    }
    if !dns_name(&config.server.public_host) {
        return Err(Error::Invalid("public_host must be a DNS name for acme"));
    }
    if config.tls.challenge == AcmeChallenge::Http01 && config.server.bind_http.port() == 80 {
        return Err(Error::Invalid(
            "bind_http must not use port 80 with the http-01 challenge",
        ));
    }
    Ok(())
}

fn http01_routes(
    resolver: &Arc<rustls_acme::ResolvesServerCertAcme>,
    config: &Config,
    fingerprint: &FingerprintSlot,
) -> Result<Loaded, Error> {
    let challenge = acme_router(Arc::clone(resolver), None);
    let redirect = Some((
        http01_addr(config)?,
        acme_router(Arc::clone(resolver), Some(redirect_base(config))),
    ));
    Ok(Loaded {
        quic: Arc::clone(resolver) as Arc<dyn ResolvesServerCert>,
        fingerprint: Arc::clone(fingerprint),
        challenge,
        redirect,
        alpn_on_https: false,
    })
}

/// Rustls configuration for the `tls-alpn-01` listener: it offers only
/// `acme-tls/1`, for which `resolver` answers with the challenge certificate.
fn challenge_config(resolver: Arc<dyn ResolvesServerCert>) -> Arc<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = vec![ACME_TLS_ALPN.to_vec()];
    Arc::new(config)
}

/// Accepts TLS-ALPN-01 validation connections until `stop` resolves. Each
/// has `limits.header_timeout` to finish its handshake and is then closed. At
/// most `limits.per_address` are open per address (further ones close on
/// accept) and [`MAX_CONNECTIONS`] in all (further accepts wait).
async fn serve_challenges(
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    limits: ServeLimits,
    stop: impl Future<Output = ()>,
) {
    let acceptor = TlsAcceptor::from(config);
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let peers = PeerSessions::default();
    peers.set_max(limits.per_address);
    let mut stop = std::pin::pin!(stop);
    let mut handshakes = tokio::task::JoinSet::new();
    loop {
        let permit = tokio::select! {
            () = &mut stop => break,
            permit = Arc::clone(&slots).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => break,
            },
        };
        let (stream, peer) = tokio::select! {
            () = &mut stop => break,
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    tracing::debug!(%error, "acme accept failed");
                    drop(permit);
                    tokio::select! {
                        () = &mut stop => break,
                        () = tokio::time::sleep(ACCEPT_BACKOFF) => {}
                    }
                    continue;
                }
            },
        };
        let Some(slot) = peers.acquire(peer.ip()) else {
            drop(stream);
            continue;
        };
        let acceptor = acceptor.clone();
        let deadline = limits.header_timeout;
        handshakes.spawn(async move {
            match tokio::time::timeout(deadline, acceptor.accept(stream)).await {
                Ok(Ok(mut stream)) => {
                    let _ = tokio::time::timeout(deadline, stream.shutdown()).await;
                }
                Ok(Err(error)) => tracing::debug!(%error, "acme handshake rejected"),
                Err(_) => tracing::debug!("acme handshake timed out"),
            }
            drop(slot);
            drop(permit);
        });
        while handshakes.try_join_next().is_some() {}
    }
    handshakes.shutdown().await;
}

/// Polls the ACME order state machine (issuance and renewal) until `stop`.
fn drive_acme(
    state: rustls_acme::AcmeState<std::io::Error>,
    cert_file: PathBuf,
    slot: FingerprintSlot,
    stop: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut state = Box::pin(state);
        while !stop.load(Ordering::Relaxed) {
            let event =
                tokio::time::timeout(Duration::from_secs(2), futures::StreamExt::next(&mut state))
                    .await;
            remember_cached(&cert_file, &slot);
            match event {
                Ok(Some(Ok(event))) => tracing::info!(?event, "acme"),
                Ok(Some(Err(error))) => tracing::warn!(%error, "acme"),
                Ok(None) => break,
                Err(_) => {}
            }
        }
    });
}

fn acme_config(config: &Config, cache: PathBuf) -> AcmeConfig<std::io::Error> {
    let host = config.server.public_host.trim().to_owned();
    let challenge = match config.tls.challenge {
        AcmeChallenge::Http01 => UseChallenge::Http01,
        AcmeChallenge::TlsAlpn01 => UseChallenge::TlsAlpn01,
    };
    AcmeConfig::new([host])
        .contact([mailto(&config.tls.acme_email)])
        .directory(directory(&config.tls))
        .cache(DirCache::new(cache))
        .challenge_type(challenge)
}

fn acme_router(
    resolver: Arc<rustls_acme::ResolvesServerCertAcme>,
    redirect: Option<String>,
) -> Router {
    let mut router =
        Router::new().route("/.well-known/acme-challenge/{token}", get(acme_challenge));
    if redirect.is_some() {
        router = router.fallback(acme_redirect);
    }
    router.with_state(Arc::new(AcmeHttp { resolver, redirect }))
}

struct AcmeHttp {
    resolver: Arc<rustls_acme::ResolvesServerCertAcme>,
    redirect: Option<String>,
}

async fn acme_challenge(
    AxumPath(token): AxumPath<String>,
    State(state): State<Arc<AcmeHttp>>,
) -> Response {
    let Some(body) = state.resolver.get_http_01_key_auth(&token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match axum::http::Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(body))
    {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn acme_redirect(uri: Uri, State(state): State<Arc<AcmeHttp>>) -> Response {
    let Some(base) = state.redirect.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = uri.path_and_query().map_or("/", |part| part.as_str());
    Redirect::permanent(&format!("{base}{path}")).into_response()
}

fn remember_cached(cert_file: &Path, slot: &FingerprintSlot) {
    let Some(fingerprint) = cached_fingerprint(cert_file) else {
        return;
    };
    if **slot.load() != Some(fingerprint) {
        slot.store(Arc::new(Some(fingerprint)));
        tracing::info!(%fingerprint, "acme certificate is ready");
    }
}

/// Leaf of the cached certificate in `cert_file`, if ACME has issued one.
fn cached_fingerprint(cert_file: &Path) -> Option<Fingerprint> {
    pem_fingerprint(&std::fs::read(cert_file).ok()?)
}

/// The cache file rustls-acme's `DirCache` writes for the configured host
/// and directory. A cache can hold others (an old host, the staging
/// directory); only this one is the certificate being served.
fn acme_cert_file(config: &Config, data_dir: &Path) -> PathBuf {
    acme_cache(data_dir).join(cert_file_name(
        config.server.public_host.trim(),
        directory(&config.tls),
    ))
}

/// `DirCache::cached_cert_file_name` for one domain: SHA-256 over each
/// domain and a NUL, then the directory URL, base64url without padding.
fn cert_file_name(domain: &str, directory: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(directory.as_bytes());
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash.finalize());
    format!("cached_cert_{encoded}")
}

fn pem_fingerprint(bytes: &[u8]) -> Option<Fingerprint> {
    let cert = CertificateDer::pem_slice_iter(bytes).next()?.ok()?;
    Some(ss_net::tls::fingerprint(cert.as_ref()))
}

fn directory(config: &config::TlsConfig) -> &str {
    if config.acme_directory.is_empty() {
        config::ACME_DIRECTORY
    } else {
        config.acme_directory.as_str()
    }
}

fn mailto(email: &str) -> String {
    let email = email.trim();
    if let Some(rest) = email.strip_prefix("mailto:") {
        format!("mailto:{rest}")
    } else {
        format!("mailto:{email}")
    }
}

fn redirect_base(config: &Config) -> String {
    let host = config.server.public_host.trim();
    let port = config.server.bind_http.port();
    if port == 443 {
        format!("https://{host}")
    } else {
        format!("https://{host}:{port}")
    }
}

fn http01_addr(config: &Config) -> Result<SocketAddr, Error> {
    if config.server.bind_http.port() == 80 {
        return Err(Error::Invalid(
            "bind_http must not use port 80 with the http-01 challenge",
        ));
    }
    let mut addr = config.server.bind_http;
    addr.set_port(80);
    Ok(addr)
}

fn acme_cache(data_dir: &Path) -> PathBuf {
    data_dir.join("tls").join("acme")
}

fn email_ok(value: &str) -> bool {
    let value = value.trim().strip_prefix("mailto:").unwrap_or(value.trim());
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.')
}

fn dns_name(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.');
    if host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok()
    {
        return false;
    }
    host.contains('.')
        && !host.starts_with('.')
        && !host.contains("..")
        && host.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '.' || character == '-'
        })
}

fn load_pem(cert_path: &Path, key_path: &Path) -> Result<TlsConfig, Error> {
    let cert_bytes = std::fs::read(cert_path)?;
    let key_bytes = std::fs::read(key_path)?;
    let certificates = CertificateDer::pem_slice_iter(&cert_bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Error::Invalid("certificate file is not valid PEM"))?;
    if certificates.is_empty() {
        return Err(Error::Invalid("certificate file has no certificate"));
    }
    let private_key = PrivateKeyDer::from_pem_slice(&key_bytes).map_err(|error| match error {
        pem::Error::NoItemsFound => Error::Invalid("private key file has no key"),
        _ => Error::Invalid("private key file is not valid PEM"),
    })?;
    Ok(TlsConfig {
        certificates,
        private_key,
    })
}

fn leaf_text(tls: &TlsConfig) -> Result<String, Error> {
    let leaf = tls
        .certificates
        .first()
        .ok_or(Error::Invalid("certificate chain is empty"))?;
    Ok(ss_net::tls::fingerprint(leaf.as_ref()).to_string())
}

fn located(data_dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        data_dir.join(path)
    }
}

fn slot_with(fingerprint: Option<Fingerprint>) -> FingerprintSlot {
    Arc::new(ArcSwap::new(Arc::new(fingerprint)))
}

fn watch_targets(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut targets = paths.to_vec();
    for path in paths {
        let Some(parent) = path.parent() else {
            continue;
        };
        if parent.as_os_str().is_empty() {
            continue;
        }
        let parent = parent.to_path_buf();
        if !targets.contains(&parent) {
            targets.push(parent);
        }
    }
    targets
}

fn spawn_watch(
    targets: Vec<PathBuf>,
    stop: Arc<AtomicBool>,
    reload: impl Fn() + Send + 'static,
) -> Result<(), Error> {
    if targets.is_empty() {
        return Ok(());
    }
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(move |_| {
            let _ = tx.send(());
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                let _ = ready_tx.send(Err(Error::Watch(error.to_string())));
                return;
            }
        };
        for target in &targets {
            if let Err(error) = watcher.watch(target, notify::RecursiveMode::NonRecursive) {
                let _ = ready_tx.send(Err(Error::Watch(error.to_string())));
                return;
            }
        }
        let _ = ready_tx.send(Ok(()));
        while !stop.load(Ordering::Relaxed) {
            if rx.recv_timeout(Duration::from_millis(50)).is_ok() {
                while rx.try_recv().is_ok() {}
                std::thread::sleep(Duration::from_millis(200));
                while rx.try_recv().is_ok() {}
                if !stop.load(Ordering::Relaxed) {
                    reload();
                }
            }
        }
    });
    match ready_rx.recv() {
        Ok(result) => result,
        Err(_) => Err(Error::Watch("watcher stopped".to_owned())),
    }
}

/// A certificate file or ACME setting the process should not start with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading or binding failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The certificate could not be parsed or installed.
    #[error(transparent)]
    Net(#[from] ss_net::Error),
    /// The operator has to change the config.
    #[error("{0}")]
    Invalid(&'static str),
    /// `notify` could not watch the certificate files.
    #[error("certificate watch: {0}")]
    Watch(String),
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use std::net::SocketAddr;

    use rustls_acme::{CertCache, caches::DirCache};
    use ss_net::tls::CertResolver;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        time::timeout,
    };

    use super::{
        ACME_TLS_ALPN, cached_fingerprint, cert_file_name, challenge_config, private_dir,
        serve_challenges,
    };
    use crate::{
        config::{ACME_DIRECTORY, AcmeChallenge, TlsMode},
        http::hardening::ServeLimits,
    };

    #[tokio::test]
    async fn idle_alpn_connections_close_at_the_handshake_deadline_and_are_capped_per_address() {
        ss_net::tls::install_default_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let identity = ss_net::tls::generate_self_signed(dir.path()).await.unwrap();
        let resolver = Arc::new(CertResolver::new(identity).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let limits = ServeLimits {
            header_timeout: Duration::from_millis(300),
            per_address: 2,
            ..ServeLimits::default()
        };
        let served = tokio::spawn(serve_challenges(
            listener,
            challenge_config(resolver),
            limits,
            async move {
                let _ = stop_rx.await;
            },
        ));

        let started = Instant::now();
        let mut first = TcpStream::connect(addr).await.unwrap();
        let mut second = TcpStream::connect(addr).await.unwrap();
        let mut third = TcpStream::connect(addr).await.unwrap();
        assert!(
            closed_within(&mut third, Duration::from_millis(200)).await,
            "a third idle connection from one address stayed open"
        );
        assert!(closed_within(&mut first, Duration::from_secs(2)).await);
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(250),
            "closed before the handshake deadline: {elapsed:?}"
        );
        assert!(closed_within(&mut second, Duration::from_secs(2)).await);

        let mut again = TcpStream::connect(addr).await.unwrap();
        assert!(
            !closed_within(&mut again, Duration::from_millis(100)).await,
            "closed connections did not free their address slots"
        );
        let _ = stop_tx.send(());
        timeout(Duration::from_secs(2), served)
            .await
            .unwrap()
            .unwrap();
    }

    /// Whether the peer closes (or resets) `stream` within `limit`.
    async fn closed_within(stream: &mut TcpStream, limit: Duration) -> bool {
        let mut buf = [0_u8; 64];
        matches!(
            timeout(limit, stream.read(&mut buf)).await,
            Ok(Ok(0) | Err(_))
        )
    }

    #[tokio::test]
    async fn the_fingerprint_comes_from_the_configured_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DirCache::new(dir.path().to_path_buf());
        let domains = vec!["chat.example.com".to_owned()];
        for n in 0..8 {
            let old =
                rcgen::generate_simple_self_signed(vec!["old.example.com".to_owned()]).unwrap();
            let directory = format!("https://staging-{n}.example/directory");
            cache
                .store_cert(&domains, &directory, old.cert.pem().as_bytes())
                .await
                .unwrap();
        }
        let wanted = rcgen::generate_simple_self_signed(domains.clone()).unwrap();
        cache
            .store_cert(&domains, ACME_DIRECTORY, wanted.cert.pem().as_bytes())
            .await
            .unwrap();
        let file = dir
            .path()
            .join(cert_file_name("chat.example.com", ACME_DIRECTORY));
        assert!(file.is_file(), "the name differs from rustls-acme's");
        assert_eq!(
            cached_fingerprint(&file),
            Some(ss_net::tls::fingerprint(wanted.cert.der()))
        );
        let missing = dir
            .path()
            .join(cert_file_name("other.example.com", ACME_DIRECTORY));
        assert_eq!(cached_fingerprint(&missing), None);
    }

    /// `init` puts HTTPS on the QUIC port number, and TLS-ALPN-01 answers on
    /// TCP at the QUIC port. With those defaults the HTTPS listener must
    /// answer the challenge itself instead of a second listener on its port.
    #[tokio::test]
    async fn tls_alpn_01_on_the_init_ports_leaves_the_https_port_to_the_https_listener() {
        ss_net::tls::install_default_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::parse(crate::config::TEMPLATE).unwrap();
        assert_eq!(
            config.server.bind_quic.port(),
            config.server.bind_http.port(),
            "this test expects init to share one port"
        );
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        config.server.bind_quic = SocketAddr::from(([127, 0, 0, 1], port));
        config.server.bind_http = config.server.bind_quic;
        config.server.public_host = "sideband.example.com".to_owned();
        config.tls.mode = TlsMode::Acme;
        config.tls.challenge = AcmeChallenge::TlsAlpn01;
        config.tls.acme_email = "ops@example.com".to_owned();
        // Unreachable: no order completes, which this test does not need.
        config.tls.acme_directory = "https://127.0.0.1:9/directory".to_owned();
        let identity = super::prepare(&config, dir.path()).await.unwrap();
        let https = tokio::net::TcpListener::bind(config.server.bind_http).await;
        assert!(
            https.is_ok(),
            "TCP {} was bound twice: {:?}",
            config.server.bind_http,
            https.err()
        );
        assert!(
            identity
                .https_config()
                .alpn_protocols
                .contains(&ACME_TLS_ALPN.to_vec()),
            "the HTTPS listener does not answer acme-tls/1"
        );
    }

    /// A validation handshake on the HTTPS listener ends after the handshake;
    /// the connection never reaches the HTTP routes.
    #[tokio::test]
    async fn the_https_listener_closes_an_acme_tls_handshake_without_serving_http() {
        ss_net::tls::install_default_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let identity = ss_net::tls::generate_self_signed(dir.path()).await.unwrap();
        let resolver = Arc::new(CertResolver::new(identity).unwrap());
        let fingerprint = resolver.fingerprint();
        let mut offered = Arc::unwrap_or_clone(super::https_config(resolver));
        offered.alpn_protocols.push(ACME_TLS_ALPN.to_vec());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let served = tokio::spawn(crate::http::hardening::serve_tls(
            listener,
            axum::Router::new().fallback(|| async { "served" }),
            Arc::new(offered),
            async move {
                let _ = stop_rx.await;
            },
        ));

        let mut client = ss_net::client::tls_config(ss_net::pins::TrustPolicy::PinnedCertificate {
            fingerprint,
        })
        .unwrap();
        client.alpn_protocols = vec![ACME_TLS_ALPN.to_vec()];
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut tls = connector.connect(name, tcp).await.unwrap();
        assert_eq!(tls.get_ref().1.alpn_protocol(), Some(ACME_TLS_ALPN));
        let _ = tls
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await;
        let mut response = Vec::new();
        let _ = timeout(Duration::from_secs(2), tls.read_to_end(&mut response))
            .await
            .expect("the validation connection stayed open");
        assert!(
            !String::from_utf8_lossy(&response).contains("200"),
            "an acme-tls/1 connection reached the HTTP routes"
        );
        let _ = stop_tx.send(());
        timeout(Duration::from_secs(2), served)
            .await
            .unwrap()
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_acme_cache_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("tls").join("acme");
        private_dir(&fresh).unwrap();
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&fresh), 0o700);
        assert_eq!(mode(&dir.path().join("tls")), 0o700);

        let existing = dir.path().join("old-acme");
        std::fs::create_dir(&existing).unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&existing).unwrap();
        assert_eq!(mode(&existing), 0o700);
    }
}
