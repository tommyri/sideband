//! Command-line host for the unauthenticated voice spike.

use clap::Parser;
use ss_net::{server::Endpoint, tls};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(version, about = "SocialSpeak voice spike (no authentication yet)")]
struct Args {
    /// Directory containing the persistent TLS identity.
    #[arg(long, default_value = "./data")]
    data_dir: PathBuf,
    /// UDP socket address for QUIC.
    #[arg(long, default_value = "0.0.0.0:443")]
    bind: SocketAddr,
    /// Exit after checking that `data-dir/socialspeak.db` already has every
    /// embedded migration. Does not apply SQL and does not listen.
    #[arg(long)]
    check_migrations: bool,
}

// The spike is I/O-bound. Keep packet handling on one executor thread; TLS
// identity file work already uses spawn_blocking in ss-net.
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let database_path = args.data_dir.join("socialspeak.db");
    if args.check_migrations {
        ss_server::db::check(&database_path).await?;
        tracing::info!(path = %database_path.display(), "migrations are current");
        return Ok(());
    }
    let database = ss_server::db::Pool::open(&database_path).await?;
    tracing::info!(path = %database.path().display(), "database ready");
    let identity = tls::generate_self_signed(&args.data_dir).await?;
    // generate_self_signed returns exactly one validated leaf certificate.
    let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
    let endpoint = Endpoint::bind(args.bind, identity)?;
    tracing::info!(address = %endpoint.local_addr()?, %fingerprint, "voice spike listening (unauthenticated)");
    let server = Arc::new(ss_server::Server::default());
    let run = Arc::clone(&server).run(endpoint.clone());
    tokio::pin!(run);
    tokio::select! {
        () = &mut run => {},
        result = tokio::signal::ctrl_c() => {
            result?;
            endpoint.close(0, b"server shutdown");
            run.await;
        }
    }
    endpoint.wait_idle().await;
    tracing::info!(stats = ?server.router().stats(), "voice spike stopped");
    Ok(())
}
