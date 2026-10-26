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
