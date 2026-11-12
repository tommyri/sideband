//! Command-line host. A subcommand is required: with none, it prints usage
//! and exits non-zero. `run` is the authenticated server
//! ([`ss_server::connection::Server`]); `spike` is the unauthenticated voice
//! spike that `ss-cli` speaks.

use clap::{Parser, Subcommand};
use ss_net::{server::Endpoint, tls};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(
    version,
    about = "Sideband server. Use `run` for the authenticated server.",
    arg_required_else_help = true
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Rebuild message search from live messages. Does not start listeners.
    ReindexSearch {
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
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
    /// Bot accounts and their tokens (`docs/bots.md`).
    Bot {
        #[command(subcommand)]
        action: BotAction,
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
    /// Start the UNAUTHENTICATED voice spike: any username joins. For
    /// `ss-cli` tests only; never expose it.
    Spike {
        /// Directory containing the database and the persistent TLS identity.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
        /// UDP socket address for QUIC.
        #[arg(long, default_value = "127.0.0.1:4433")]
        bind: SocketAddr,
    },
    /// Exit non-zero unless `data-dir/socialspeak.db` already has every
    /// embedded migration. Does not apply SQL and does not listen.
    CheckMigrations {
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
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
    /// Replace a user's password hash and end every session of the account.
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
    /// List or end an account's login sessions.
    Sessions {
        #[command(subcommand)]
        action: SessionsAction,
    },
}

#[derive(Subcommand)]
enum SessionsAction {
    /// Print the account's live sessions, oldest first. Tokens are never shown.
    List {
        /// Login name. Matching is case-insensitive.
        #[arg(long)]
        username: String,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// End one session or all of them. The token stops working at once; a
    /// running server closes its connections within 30 seconds.
    Revoke {
        /// Login name. Matching is case-insensitive.
        #[arg(long)]
        username: String,
        /// Session id from `user sessions list`.
        #[arg(long, required_unless_present = "all", conflicts_with = "all")]
        id: Option<String>,
        /// End every session of the account.
        #[arg(long)]
        all: bool,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum BotAction {
    /// Create a bot account and print its token once. The operator is the
    /// issuer, so the scopes are not limited to a member's bits.
    Create {
        /// 3 to 32 characters of a-z, 0-9, underscore, or dot.
        #[arg(long)]
        username: String,
        /// Name shown in the member list. Defaults to the username.
        #[arg(long, default_value = "")]
        display_name: String,
        /// Permission names the token may use, comma-separated. The bot holds
        /// only those its roles also grant.
        #[arg(long, default_value = "VIEW_CHANNEL,SEND_MESSAGES")]
        scopes: String,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// Print every bot, its scopes and whether it has a live token.
    List {
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// Replace a bot's token and print the new one. The old one stops at
    /// once; a running server closes its connections within 30 seconds.
    Reset {
        /// The bot's username.
        #[arg(long)]
        username: String,
        /// Permission names for the new token, comma-separated.
        #[arg(long, default_value = "VIEW_CHANNEL,SEND_MESSAGES")]
        scopes: String,
        /// Directory containing `socialspeak.db`.
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
    },
    /// Revoke a bot's token without issuing another.
    Revoke {
        /// The bot's username.
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
    ss_net::tls::install_default_crypto_provider();
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
    match Args::parse().command {
        Command::ReindexSearch { data_dir } => {
            let pool = ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await?;
            let mut writer = pool.write().await;
            ss_server::db::search::reindex(&mut writer).await?;
            println!("message search index rebuilt");
        }
        Command::Conn {
            action: ConnAction::List { data_dir },
        } => {
            let config = ss_server::config::load(&data_dir)?;
            let target = inspection_addr(config.internal.bind)?;
            let response = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(5))
                .build()?
                .get(format!("http://{target}/connections"))
                .send()
                .await?
                .error_for_status()?;
            let rows: Vec<serde_json::Value> = serde_json::from_slice(&response.bytes().await?)?;
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        Command::Init { data_dir } => ss_server::serve::init(data_dir).await?,
        Command::Run { data_dir } => ss_server::serve::run(data_dir).await?,
        Command::CheckConfig { data_dir } => {
            ss_server::serve::check(&data_dir)?;
            println!("config ok");
        }
        Command::Fingerprint { data_dir } => ss_server::serve::fingerprint(data_dir).await?,
        Command::Tls {
            action: TlsAction::Rotate { data_dir },
        } => ss_server::serve::rotate(data_dir).await?,
        Command::Backup { path, data_dir } => {
            ss_server::serve::backup(data_dir, path).await?;
        }
        Command::Spike { data_dir, bind } => {
            stderr_logs();
            spike(data_dir, bind).await?;
        }
        Command::CheckMigrations { data_dir } => {
            stderr_logs();
            let database_path = data_dir.join("socialspeak.db");
            ss_server::db::check(&database_path).await?;
            tracing::info!(path = %database_path.display(), "migrations are current");
        }
        Command::Invite {
            action: InviteAction::Create { owner, data_dir },
        } => {
            stderr_logs();
            create_owner_invite(owner, data_dir).await?;
        }
        Command::User { action } => {
            stderr_logs();
            user(action).await?;
        }
        Command::Bot { action } => {
            stderr_logs();
            bot(action).await?;
        }
        Command::Role {
            action: RoleAction::GrantAdmin { username, data_dir },
        } => {
            stderr_logs();
            grant_admin(data_dir, &username).await?;
        }
    }
    Ok(())
}

/// Where `conn list` sends its request. A wildcard internal bind, which a
/// container needs so Docker can publish the port, is reached on the same
/// host's loopback. Any other non-loopback address is refused, so the request
/// never leaves the machine.
fn inspection_addr(bind: SocketAddr) -> anyhow::Result<SocketAddr> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    anyhow::ensure!(
        ip.is_loopback(),
        "connection inspection requires a loopback or wildcard internal listener"
    );
    Ok(SocketAddr::new(ip, bind.port()))
}

/// `tracing` to stderr, filtered by `RUST_LOG` (default `info`).
fn stderr_logs() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
}

async fn user(action: UserAction) -> anyhow::Result<()> {
    match action {
        UserAction::List { data_dir } => list_users(data_dir).await,
        UserAction::SetPassword {
            username,
            password,
            data_dir,
        } => reset_password(data_dir, &username, &password).await,
        UserAction::Ban {
            username,
            reason,
            data_dir,
        } => ban_user(data_dir, &username, reason).await,
        UserAction::Unban { username, data_dir } => unban_user(data_dir, &username).await,
        UserAction::Sessions { action } => user_sessions(action).await,
    }
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
    let ended = ss_server::auth::set_password(&pool, username, password).await?;
    println!("password updated");
    println!("{ended} session(s) ended");
    Ok(())
}

async fn user_sessions(action: SessionsAction) -> anyhow::Result<()> {
    match action {
        SessionsAction::List { username, data_dir } => list_sessions(data_dir, &username).await,
        SessionsAction::Revoke {
            username,
            id,
            all: _,
            data_dir,
        } => revoke_sessions(data_dir, &username, id.as_deref()).await,
    }
}

async fn list_sessions(data_dir: PathBuf, username: &str) -> anyhow::Result<()> {
    let pool = ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await?;
    let sessions = match ss_server::auth::list_sessions(&pool, username).await {
        Err(ss_server::auth::Error::NotFound) => anyhow::bail!("no account named {username}"),
        other => other?,
    };
    for session in sessions {
        let expires = session.expires_at.map_or_else(|| "never".to_owned(), utc);
        println!(
            "{}  {}  created {}  last seen {}  expires {}",
            session.id,
            session.device_name,
            utc(session.created_at),
            utc(session.last_seen_at),
            expires,
        );
    }
    Ok(())
}

async fn revoke_sessions(
    data_dir: PathBuf,
    username: &str,
    id: Option<&str>,
) -> anyhow::Result<()> {
    let session = id
        .map(|id| {
            id.parse::<ss_core::Id>()
                .map_err(|_| anyhow::anyhow!("--id must be a session id from `user sessions list`"))
        })
        .transpose()?;
    let pool = ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await?;
    match ss_server::auth::revoke_sessions(&pool, username, session).await {
        Ok(ended) => {
            println!("{ended} session(s) ended");
            Ok(())
        }
        Err(ss_server::auth::Error::NotFound) => {
            anyhow::bail!("no such account, or no such session for it")
        }
        Err(error) => Err(error.into()),
    }
}

async fn bot(action: BotAction) -> anyhow::Result<()> {
    use ss_server::handlers::bots;
    let open = |data_dir: PathBuf| async move {
        ss_server::db::Pool::open(data_dir.join("socialspeak.db")).await
    };
    match action {
        BotAction::Create {
            username,
            display_name,
            scopes,
            data_dir,
        } => {
            let scopes = parse_scopes(&scopes)?;
            let pool = open(data_dir).await?;
            let (user, _, token) =
                bots::create_account(&pool, None, &username, &display_name, scopes).await?;
            println!("bot {} ({}) created", user.username, user.id);
            println!("token (shown once): {}", *token);
        }
        BotAction::List { data_dir } => {
            let pool = open(data_dir).await?;
            for (user, token) in bots::list_accounts(&pool).await? {
                let state = token.map_or_else(
                    || "no live token".to_owned(),
                    |token| {
                        format!(
                            "scopes {:?}, issued {}",
                            token.scopes,
                            utc(token.created_at)
                        )
                    },
                );
                println!("{}  {}  {state}", user.id, user.username);
            }
        }
        BotAction::Reset {
            username,
            scopes,
            data_dir,
        } => {
            let scopes = parse_scopes(&scopes)?;
            let pool = open(data_dir).await?;
            let bot = bots::find(&pool, &username).await?;
            let (_, _, token) = bots::replace_token(&pool, None, bot.id, scopes).await?;
            println!("token (shown once): {}", *token);
        }
        BotAction::Revoke { username, data_dir } => {
            let pool = open(data_dir).await?;
            let bot = bots::find(&pool, &username).await?;
            let _revoked = bots::revoke_token(&pool, None, bot.id).await?;
            println!("token revoked");
        }
    }
    Ok(())
}

/// `VIEW_CHANNEL,SEND_MESSAGES` (names from `03-data-model.md`) as bits.
fn parse_scopes(text: &str) -> anyhow::Result<ss_core::Permissions> {
    let mut scopes = ss_core::Permissions::empty();
    for name in text
        .split([',', '|'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        scopes |= ss_core::Permissions::from_name(&name.to_ascii_uppercase())
            .ok_or_else(|| anyhow::anyhow!("unknown permission {name}"))?;
    }
    Ok(scopes)
}

/// `YYYY-MM-DD HH:MM UTC` for a Unix-millisecond timestamp.
fn utc(at: ss_core::Timestamp) -> String {
    let seconds = at.as_millis().div_euclid(1000);
    let (days, of_day) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Howard Hinnant's days-to-civil, proleptic Gregorian.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        of_day / 3600,
        of_day % 3600 / 60
    )
}

/// The unauthenticated T-016 voice spike, until Ctrl-C.
async fn spike(data_dir: PathBuf, bind: SocketAddr) -> anyhow::Result<()> {
    let database_path = data_dir.join("socialspeak.db");
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

#[cfg(test)]
mod inspection_tests {
    use super::inspection_addr;

    #[test]
    fn loopback_and_wildcard_binds_are_inspected_on_loopback() {
        for (bind, target) in [
            ("127.0.0.1:9101", "127.0.0.1:9101"),
            ("0.0.0.0:9101", "127.0.0.1:9101"),
            ("[::1]:9101", "[::1]:9101"),
            ("[::]:9101", "[::1]:9101"),
        ] {
            assert_eq!(
                inspection_addr(bind.parse().unwrap()).unwrap(),
                target.parse().unwrap(),
                "{bind}"
            );
        }
    }

    #[test]
    fn other_addresses_are_refused() {
        for bind in ["10.0.0.5:9101", "192.0.2.1:9101", "[2001:db8::1]:9101"] {
            assert!(inspection_addr(bind.parse().unwrap()).is_err(), "{bind}");
        }
    }
}
