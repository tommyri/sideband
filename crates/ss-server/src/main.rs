//! Command-line host. The default listener is the unauthenticated voice spike.
//! Authenticated Hello is [`ss_server::connection::Server`].

use clap::{Parser, Subcommand};
use ss_net::{server::Endpoint, tls};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(
    version,
    about = "Sideband server. The default listener is the unauthenticated voice spike.",
    args_conflicts_with_subcommands = true
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Directory containing the database and the persistent TLS identity.
    #[arg(long, default_value_os_t = default_data_dir())]
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
    /// Inspect live connections through the loopback operator listener.
    Conn {
        #[command(subcommand)]
        action: ConnAction,
    },
    /// Write `config.toml`, the database, and a TLS identity.
    Init {
        /// Directory for the config, database, attachments, and TLS identity.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// Serve authenticated QUIC and HTTP until SIGTERM or Ctrl-C.
    Run {
        /// Directory containing `config.toml`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// Exit non-zero when `config.toml` has an unknown key or a bad value.
    CheckConfig {
        /// Directory containing `config.toml`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// Certificate maintenance.
    Tls {
        #[command(subcommand)]
        action: TlsAction,
    },
    /// Print the certificate fingerprint.
    Fingerprint {
        /// Directory containing the TLS identity.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// Write a tar of the database and attachments.
    Backup {
        /// Destination tar path.
        path: PathBuf,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
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
    /// Manage roles.
    Role {
        #[command(subcommand)]
        action: RoleAction,
    },
}

#[derive(Subcommand)]
enum ConnAction {
    /// Print current QUIC path counters as JSON (no credentials or addresses).
    List {
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum TlsAction {
    /// Replace the self-signed identity. Connected clients re-prompt.
    Rotate {
        /// Directory containing `config.toml` and `tls/identity.json`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
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
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum UserAction {
    /// Print every username.
    List {
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// Replace a user's password hash.
    SetPassword {
        /// Login name. Matching is case-insensitive.
        #[arg(long)]
        username: String,
        /// New password. It is hashed and not printed.
        #[arg(long)]
        password: String,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// Record a permanent ban. A live session stays connected until restart.
    Ban {
        /// Login name. Matching is case-insensitive.
        #[arg(long)]
        username: String,
        /// Reason stored on the ban and the audit row.
        #[arg(long)]
        reason: Option<String>,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// Lift every open ban for an account.
    Unban {
        /// Login name. Matching is case-insensitive.
        #[arg(long)]
        username: String,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum RoleAction {
    /// Grant the seeded Admin role. A running server sees it after restart.
    GrantAdmin {
        /// Login name. Matching is case-insensitive.
        #[arg(long)]
        username: String,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
}

fn default_data_dir() -> PathBuf {
    ss_server::config::default_data_dir()
}

/// Runtime worker override. Unset, 0, or 1 keeps the single-threaded runtime.
const WORKER_THREADS: &str = "SS_WORKER_THREADS";

// The server is I/O-bound. By default packet handling stays on one executor
// thread; TLS identity file work and password hashing use spawn_blocking.
// `SS_WORKER_THREADS=N` (N > 1) selects a multi-threaded runtime for load
// experiments; docs/plan/notes/load-test.md records the measured effect.
fn main() -> anyhow::Result<()> {
    let workers = std::env::var(WORKER_THREADS)
        .ok()
        .map(|value| value.trim().parse::<usize>())
        .transpose()
        .map_err(|_| anyhow::anyhow!("{WORKER_THREADS} must be a whole number"))?
        .unwrap_or(1);
    let runtime = if workers > 1 {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .build()?
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
    };
    runtime.block_on(start())
}

async fn start() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command {
        Some(Command::Conn {
            action: ConnAction::List { data_dir },
        }) => {
            let config = ss_server::config::load(&data_dir)?;
            anyhow::ensure!(
                config.internal.bind.ip().is_loopback(),
                "connection inspection requires a loopback internal listener"
            );
            let response = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(5))
                .build()?
                .get(format!("http://{}/connections", config.internal.bind))
                .send()
                .await?
                .error_for_status()?;
            let rows: Vec<serde_json::Value> = serde_json::from_slice(&response.bytes().await?)?;
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        Some(Command::Init { data_dir }) => ss_server::serve::init(data_dir).await?,
        Some(Command::Run { data_dir }) => ss_server::serve::run(data_dir).await?,
        Some(Command::CheckConfig { data_dir }) => {
            ss_server::serve::check(&data_dir)?;
            println!("config ok");
        }
        Some(Command::Fingerprint { data_dir }) => ss_server::serve::fingerprint(data_dir).await?,
        Some(Command::Tls {
            action: TlsAction::Rotate { data_dir },
        }) => ss_server::serve::rotate(data_dir).await?,
        Some(Command::Backup { path, data_dir }) => {
            ss_server::serve::backup(data_dir, path).await?;
        }
        other => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .with_writer(std::io::stderr)
                .init();
            match other {
                Some(Command::Invite {
                    action: InviteAction::Create { owner, data_dir },
                }) => create_owner_invite(owner, data_dir).await?,
                Some(Command::User {
                    action: UserAction::List { data_dir },
                }) => list_users(data_dir).await?,
                Some(Command::User {
                    action:
                        UserAction::SetPassword {
                            username,
                            password,
                            data_dir,
                        },
                }) => reset_password(data_dir, &username, &password).await?,
                Some(Command::User {
                    action:
                        UserAction::Ban {
                            username,
                            reason,
                            data_dir,
                        },
                }) => ban_user(data_dir, &username, reason).await?,
                Some(Command::User {
                    action: UserAction::Unban { username, data_dir },
                }) => unban_user(data_dir, &username).await?,
                Some(Command::Role {
                    action: RoleAction::GrantAdmin { username, data_dir },
                }) => grant_admin(data_dir, &username).await?,
                Some(
                    Command::Conn { .. }
                    | Command::Init { .. }
                    | Command::Run { .. }
                    | Command::CheckConfig { .. }
                    | Command::Fingerprint { .. }
                    | Command::Backup { .. }
                    | Command::Tls { .. },
                )
                | None => serve(args.data_dir, args.bind, args.check_migrations).await?,
            }
        }
    }
    Ok(())
}

async fn list_users(data_dir: PathBuf) -> anyhow::Result<()> {
    let pool = ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await?;
    let mut conn = pool.write().await;
    for user in ss_server::db::users::list(&mut conn).await? {
        println!("{}", user.username);
    }
    Ok(())
}

async fn create_owner_invite(owner: bool, data_dir: PathBuf) -> anyhow::Result<()> {
    if !owner {
        anyhow::bail!("pass --owner to create the bootstrap invite");
    }
    let pool = ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await?;
    match ss_server::auth::create_owner_invite(&pool).await {
        Ok(code) => {
            // stdout only: the code grants Admin and must not reach log files.
            println!("owner invite: {code}");
            Ok(())
        }
        Err(ss_server::auth::Error::OwnerExists) => {
            anyhow::bail!("an owner is already registered")
        }
        Err(error) => Err(error.into()),
    }
}

async fn ban_user(data_dir: PathBuf, username: &str, reason: Option<String>) -> anyhow::Result<()> {
    let pool = ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await?;
    ss_server::handlers::ban_account(&pool, username, reason.as_deref()).await?;
    println!("banned {username}");
    Ok(())
}

async fn unban_user(data_dir: PathBuf, username: &str) -> anyhow::Result<()> {
    let pool = ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await?;
    ss_server::handlers::unban_account(&pool, username).await?;
    println!("unbanned {username}");
    Ok(())
}

async fn grant_admin(data_dir: PathBuf, username: &str) -> anyhow::Result<()> {
    let pool = ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await?;
    if ss_server::handlers::grant_admin(&pool, username).await? {
        println!("admin granted");
    } else {
        println!("already an admin");
    }
    Ok(())
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
        // stdout only: the code grants Admin and must not reach log files.
        println!("owner invite: {code}");
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
