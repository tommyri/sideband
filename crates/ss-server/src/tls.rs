//! Certificate modes for the authenticated listener.
//!
//! `self-signed` reuses `tls/identity.json`. `files` reloads PEM when the
//! files change. `acme` orders a certificate for `public_host` and serves
//! that same leaf on QUIC. Reloads swap the rustls resolver. The UDP socket
//! stays bound.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
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
use futures::Stream;
use notify::Watcher;
use rustls::server::ResolvesServerCert;
use rustls_acme::{AcmeConfig, UseChallenge, caches::DirCache};
use ss_net::tls::{CertResolver, Fingerprint, TlsConfig};
use tokio::net::TcpListener;

use crate::config::{self, AcmeChallenge, Config, TlsMode};

type FingerprintSlot = Arc<ArcSwap<Option<Fingerprint>>>;

/// Live certificate. Drop stops the file watcher.
pub struct Identity {
    quic: Arc<dyn ResolvesServerCert>,
    fingerprint: FingerprintSlot,
    challenge: Router,
    redirect: Option<(SocketAddr, Router)>,
    stop: Arc<AtomicBool>,
}

struct Loaded {
    quic: Arc<dyn ResolvesServerCert>,
    fingerprint: FingerprintSlot,
    challenge: Router,
    redirect: Option<(SocketAddr, Router)>,
}

impl Identity {
    /// Resolver installed in the QUIC endpoint.
    pub fn resolver(&self) -> Arc<dyn ResolvesServerCert> {
        Arc::clone(&self.quic)
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
        TlsMode::Acme => cached_fingerprint(&acme_cache(data_dir))
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
    std::fs::create_dir_all(&cache)?;
    let built = acme_config(config, cache.clone());
    let fingerprint = slot_with(cached_fingerprint(&cache));
    let mut loaded = match config.tls.challenge {
        AcmeChallenge::Http01 => start_http01(built, config, cache.clone(), &fingerprint, stop),
        AcmeChallenge::TlsAlpn01 => start_alpn(built, config.server.bind_quic).await?,
    };
    loaded.fingerprint = fingerprint;
    if cached_fingerprint(&cache).is_none() {
        tracing::info!("acme has not issued a certificate yet");
    }
    let slot = Arc::clone(&loaded.fingerprint);
    let stopped = Arc::clone(stop);
    tokio::spawn(async move {
        while !stopped.load(Ordering::Relaxed) {
            remember_cached(&cache, &slot);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    Ok(loaded)
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
    Ok(())
}

fn start_http01(
    built: AcmeConfig<std::io::Error>,
    config: &Config,
    cache: PathBuf,
    fingerprint: &FingerprintSlot,
    stop: &Arc<AtomicBool>,
) -> Loaded {
    let state = built.state();
    let resolver = state.resolver();
    drive_http01(state, cache, Arc::clone(fingerprint), Arc::clone(stop));
    let challenge = acme_router(Arc::clone(&resolver), None);
    let redirect = redirect_addr(config).map(|addr| {
        (
            addr,
            acme_router(Arc::clone(&resolver), Some(redirect_base(config))),
        )
    });
    Loaded {
        quic: resolver,
        fingerprint: Arc::clone(fingerprint),
        challenge,
        redirect,
    }
}

async fn start_alpn(built: AcmeConfig<std::io::Error>, bind: SocketAddr) -> Result<Loaded, Error> {
    let listener = TcpListener::bind(bind).await?;
    let state = built.state();
    let resolver = state.resolver();
    let mut incoming =
        Box::pin(state.tokio_incoming(TcpAccept(listener), vec![b"acme-tls/1".to_vec()]));
    tokio::spawn(async move {
        loop {
            let next = std::future::poll_fn(|cx| incoming.as_mut().poll_next(cx)).await;
            match next {
                Some(Ok(mut stream)) => {
                    let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
                }
                Some(Err(error)) => tracing::warn!(%error, "acme handshake"),
                None => break,
            }
        }
    });
    Ok(Loaded {
        quic: resolver,
        fingerprint: slot_with(None),
        challenge: Router::new(),
        redirect: None,
    })
}

fn drive_http01(
    state: rustls_acme::AcmeState<std::io::Error>,
    cache: PathBuf,
    slot: FingerprintSlot,
    stop: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut state = Box::pin(state);
        while !stop.load(Ordering::Relaxed) {
            let event = tokio::time::timeout(
                Duration::from_secs(2),
                std::future::poll_fn(|cx| state.as_mut().poll_next(cx)),
            )
            .await;
            remember_cached(&cache, &slot);
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

fn remember_cached(cache: &Path, slot: &FingerprintSlot) {
    let Some(fingerprint) = cached_fingerprint(cache) else {
        return;
    };
    if **slot.load() != Some(fingerprint) {
        slot.store(Arc::new(Some(fingerprint)));
        tracing::info!(%fingerprint, "acme certificate is ready");
    }
}

fn cached_fingerprint(dir: &Path) -> Option<Fingerprint> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("cached_cert_") {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if let Some(fingerprint) = pem_fingerprint(&bytes) {
            return Some(fingerprint);
        }
    }
    None
}

fn pem_fingerprint(bytes: &[u8]) -> Option<Fingerprint> {
    let cert = rustls_pemfile::certs(&mut &bytes[..]).next()?.ok()?;
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

fn redirect_addr(config: &Config) -> Option<SocketAddr> {
    if !config.tls.http_redirect || config.server.bind_http.port() == 80 {
        return None;
    }
    let mut addr = config.server.bind_http;
    addr.set_port(80);
    Some(addr)
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
    let certificates =
        rustls_pemfile::certs(&mut cert_bytes.as_slice()).collect::<Result<Vec<_>, _>>()?;
    if certificates.is_empty() {
        return Err(Error::Invalid("certificate file has no certificate"));
    }
    let private_key = rustls_pemfile::private_key(&mut key_bytes.as_slice())?
        .ok_or(Error::Invalid("private key file has no key"))?;
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

struct TcpAccept(TcpListener);

impl Stream for TcpAccept {
    type Item = std::io::Result<tokio::net::TcpStream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut().0.poll_accept(cx) {
            Poll::Ready(Ok((stream, _))) => Poll::Ready(Some(Ok(stream))),
            Poll::Ready(Err(error)) => Poll::Ready(Some(Err(error))),
            Poll::Pending => Poll::Pending,
        }
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
