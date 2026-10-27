//! `init` and `run`. The library does not bind sockets until [`run`].

use std::{path::PathBuf, sync::Arc, time::Duration};

use metrics_exporter_prometheus::PrometheusBuilder;
use ss_net::{server::Endpoint, tls};
use tokio::{net::TcpListener, sync::watch, time::timeout};
use tracing_subscriber::{EnvFilter, fmt::writer::MakeWriterExt};

use crate::{
    auth, backup,
    config::{self, Config, TlsMode},
    connection,
    db::{self, Pool, settings},
    http,
    storage::UploadPolicy,
};

/// `init`: create the config, database, and TLS identity, then print the
/// owner invite and fingerprint.
pub async fn init(data_dir: PathBuf) -> Result<(), Error> {
    let path = config::ensure_file(&data_dir)?;
    let config = config::load(&data_dir)?;
    let _logs = init_logging(&data_dir, config.logging.json);
    let pool = Pool::open(data_dir.join("socialspeak.db")).await?;
    apply_settings(&pool, &config).await?;
    print_invite(&pool).await?;
    let fingerprint = install_identity(&data_dir).await?;
    println!("fingerprint: {fingerprint}");
    println!("config: {}", path.display());
    Ok(())
}

/// Loads the config and returns it. A typo'd key fails.
pub fn check(data_dir: &std::path::Path) -> Result<Config, Error> {
    Ok(config::load(data_dir)?)
}

/// Prints the certificate fingerprint, creating a self-signed identity when
/// the data directory does not have one yet.
pub async fn fingerprint(data_dir: PathBuf) -> Result<(), Error> {
    let fingerprint = install_identity(&data_dir).await?;
    println!("fingerprint: {fingerprint}");
    Ok(())
}

/// Online database snapshot plus the attachments tree.
pub async fn backup(data_dir: PathBuf, destination: PathBuf) -> Result<(), Error> {
    let pool = Pool::open(data_dir.join("socialspeak.db")).await?;
    backup::write(&pool, &data_dir, &destination).await?;
    println!("backup: {}", destination.display());
    Ok(())
}

/// Authenticated QUIC, public HTTP, and the internal listener.
///
/// SIGTERM and Ctrl-C notice connected clients, close them, and checkpoint
/// SQLite. The drain is bounded so a stuck peer cannot hold the process.
pub async fn run(data_dir: PathBuf) -> Result<(), Error> {
    config::ensure_file(&data_dir)?;
    let config = config::load(&data_dir)?;
    let _logs = init_logging(&data_dir, config.logging.json);
    if config.tls.mode != TlsMode::SelfSigned {
        return Err(Error::TlsMode);
    }
    let metrics = PrometheusBuilder::new()
        .install_recorder()
        .map_err(|_| Error::Metrics)?;
    let pool = Arc::new(Pool::open(data_dir.join("socialspeak.db")).await?);
    apply_settings(&pool, &config).await?;
    print_invite(&pool).await?;
    let server = open_server(&pool, &config).await?;
    let identity = tls::generate_self_signed(&data_dir).await?;
    let leaf = identity.certificates.first().ok_or(Error::Identity)?;
    let fingerprint = tls::fingerprint(leaf.as_ref());
    println!("fingerprint: {fingerprint}");
    let endpoint = Endpoint::bind(config.server.bind_quic, identity)?;
    let public = TcpListener::bind(config.server.bind_http).await?;
    let internal = TcpListener::bind(config.internal.bind).await?;
    println!(
        "listening quic={} http={} internal={}",
        endpoint.local_addr()?,
        public.local_addr()?,
        internal.local_addr()?
    );
    tracing::info!(
        public_host = %config.server.public_host,
        %fingerprint,
        "authenticated server listening"
    );
    let (stop_tx, stop_rx) = watch::channel(false);
    let quic = tokio::spawn(Arc::clone(&server).run(endpoint.clone()));
    let site = tokio::spawn(listen(public, server.public_routes(), stop_rx.clone()));
    let probe = tokio::spawn(listen(
        internal,
        http::internal::router(Arc::clone(&server), metrics),
        stop_rx,
    ));
    wait_for_stop().await;
    server.shutdown();
    endpoint.close(crate::registry::CLOSE_SHUTDOWN, b"server shutdown");
    let _ = stop_tx.send(true);
    let _ = timeout(Duration::from_millis(1500), quic).await;
    let _ = timeout(Duration::from_millis(200), site).await;
    let _ = timeout(Duration::from_millis(200), probe).await;
    pool.checkpoint().await?;
    tracing::info!("server stopped");
    Ok(())
}

async fn open_server(pool: &Arc<Pool>, config: &Config) -> Result<Arc<connection::Server>, Error> {
    let policy = UploadPolicy {
        max_bytes: config.limits.upload_max_bytes,
        ..UploadPolicy::default()
    };
    Ok(Arc::new(
        connection::Server::open_with(Arc::clone(pool), policy).await?,
    ))
}

async fn listen(listener: TcpListener, app: axum::Router, mut stop: watch::Receiver<bool>) {
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = stop.changed().await;
    });
    if let Err(error) = server.await {
        tracing::debug!(%error, "http listener stopped");
    }
}

async fn print_invite(pool: &Pool) -> Result<(), Error> {
    if let Some(code) = auth::ensure_owner_invite(pool).await? {
        println!("owner invite: {code}");
    }
    Ok(())
}

async fn apply_settings(pool: &Pool, config: &Config) -> Result<(), Error> {
    let mut conn = pool.write().await;
    settings::set(&mut conn, settings::NAME, &config.server.name).await?;
    let enabled = if config.previews.enabled { "on" } else { "off" };
    settings::set(&mut conn, "link_previews", enabled).await?;
    settings::set(&mut conn, "link_preview_allow", &config.previews.allow).await?;
    settings::set(&mut conn, "link_preview_deny", &config.previews.deny).await?;
    Ok(())
}

async fn install_identity(data_dir: &std::path::Path) -> Result<String, Error> {
    let identity = tls::generate_self_signed(data_dir).await?;
    let leaf = identity.certificates.first().ok_or(Error::Identity)?;
    Ok(tls::fingerprint(leaf.as_ref()).to_string())
}

/// File rotation under `logs/` plus stderr. The guard flushes on drop.
pub fn init_logging(
    data_dir: &std::path::Path,
    json: bool,
) -> tracing_appender::non_blocking::WorkerGuard {
    let directory = data_dir.join("logs");
    let _ = std::fs::create_dir_all(&directory);
    let rolling = tracing_appender::rolling::daily(directory, "socialspeak.log");
    let (file, guard) = tracing_appender::non_blocking(rolling);
    let writer = std::io::stderr.and(file);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_writer(writer)
            .with_ansi(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(writer)
            .with_ansi(false)
            .init();
    }
    guard
}

async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                tracing::debug!(%error, "SIGTERM is not available");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    tracing::debug!(%error, "ctrl-c");
                }
            }
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Startup or shutdown failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The config file is missing or invalid.
    #[error(transparent)]
    Config(#[from] config::Error),
    /// SQLite failed.
    #[error(transparent)]
    Database(#[from] db::Error),
    /// The owner invite could not be prepared.
    #[error(transparent)]
    Auth(#[from] auth::Error),
    /// QUIC or the certificate could not be loaded.
    #[error(transparent)]
    Transport(#[from] ss_net::Error),
    /// A socket could not be bound.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The tar could not be written.
    #[error(transparent)]
    Backup(#[from] backup::Error),
    /// The identity had no leaf certificate.
    #[error("tls identity has no certificate")]
    Identity,
    /// `acme` and `files` wait for the TLS task.
    #[error("only tls mode self-signed can listen")]
    TlsMode,
    /// A recorder was already installed in this process.
    #[error("metrics recorder is already installed")]
    Metrics,
}
