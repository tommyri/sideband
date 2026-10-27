//! Command-line host. The default listener is the unauthenticated voice spike.
//! Authenticated Hello is [`ss_server::connection::Server`].

use clap::{Parser, Subcommand};
use ss_net::{server::Endpoint, tls};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(
    version,
    about = "SocialSpeak server. The default listener is the unauthenticated voice spike.",
    args_conflicts_with_subcommands = true
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Directory containing the database and the persistent TLS identity.
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

#[derive(Subcommand)]
enum Command {
    /// Create an invite code.
    Invite {
        #[command(subcommand)]
        action: InviteAction,
    },
    /// Manage accounts.
    User {
        #[command(subcommand)]
        action: UserAction,
    },
}

#[derive(Subcommand)]
enum InviteAction {
    /// Print a one-time owner invite. Fails once a user exists.
    Create {
        /// Rotate the owner bootstrap code.
        #[arg(long)]
        owner: bool,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value = "./data")]
        data_dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum UserAction {
    /// Replace a user's password hash.
    SetPassword {
        /// Login name. Matching is case-insensitive.
        #[arg(long)]
        username: String,
        /// New password. It is hashed and not printed.
        #[arg(long)]
        password: String,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value = "./data")]
        data_dir: PathBuf,
    },
}

// The spike is I/O-bound. Keep packet handling on one executor thread; TLS
// identity file work and password hashing use spawn_blocking.
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    match args.command {
        Some(Command::Invite {
            action: InviteAction::Create { owner, data_dir },
        }) => create_owner_invite(owner, data_dir).await,
        Some(Command::User {
            action:
                UserAction::SetPassword {
                    username,
                    password,
                    data_dir,
                },
        }) => reset_password(data_dir, &username, &password).await,
        None => serve(args.data_dir, args.bind, args.check_migrations).await,
    }
}

async fn create_owner_invite(owner: bool, data_dir: PathBuf) -> anyhow::Result<()> {
    if !owner {
        anyhow::bail!("pass --owner to create the bootstrap invite");
    }
    let pool = ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await?;
    match ss_server::auth::create_owner_invite(&pool).await {
        Ok(code) => {
            println!("owner invite: {code}");
            tracing::info!("owner invite: {code}");
            Ok(())
        }
        Err(ss_server::auth::Error::OwnerExists) => {
            anyhow::bail!("an owner is already registered")
        }
        Err(error) => Err(error.into()),
    }
}

async fn reset_password(data_dir: PathBuf, username: &str, password: &str) -> anyhow::Result<()> {
    let pool = ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await?;
    ss_server::auth::set_password(&pool, username, password).await?;
    println!("password updated");
    Ok(())
}

async fn serve(data_dir: PathBuf, bind: SocketAddr, check_migrations: bool) -> anyhow::Result<()> {
    let database_path = data_dir.join("socialspeak.db");
    if check_migrations {
        ss_server::db::check(&database_path).await?;
        tracing::info!(path = %database_path.display(), "migrations are current");
        return Ok(());
    }
    let database = ss_server::db::Pool::open(&database_path).await?;
    tracing::info!(path = %database.path().display(), "database ready");
    if let Some(code) = ss_server::auth::ensure_owner_invite(&database).await? {
        println!("owner invite: {code}");
        tracing::info!("owner invite: {code}");
    }
    let identity = tls::generate_self_signed(&data_dir).await?;
    // generate_self_signed returns exactly one validated leaf certificate.
    let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
    let endpoint = Endpoint::bind(bind, identity)?;
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
