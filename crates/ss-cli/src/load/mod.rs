//! `ss-cli load`: N synthetic users across M voice channels against an
//! authenticated `ss-server run`.
//!
//! Each bot talks in bursts of pre-encoded Opus, posts text at a rate, may
//! leave and rejoin voice, and reconnects with its session token after a
//! storm or a lost session. Datagram timestamps carry the process send
//! clock, so receivers measure relay delay through the server.

mod bot;
#[cfg(test)]
mod it;
mod plan;
mod session;
mod state;
mod stats;

use std::{
    collections::BTreeMap,
    fmt,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use ss_core::{Id, VoiceProfile};
use ss_proto::{
    v1::{self, client_envelope},
    validate::id_from_bytes,
};
use tokio::{sync::watch, task::JoinSet};

pub use plan::{Behavior, PlanError};
pub use session::{SessionError, Target};
pub use state::StateError;

use crate::{bots, graph::GraphError};
use session::{Credential, Session};
use state::{Account, State};
use stats::{Snapshot, Stats};

/// Name of the text channel the bots post to.
pub const TEXT_CHANNEL: &str = "load-text";
/// Wait after a `RATE_LIMITED` registration. The server refills one slot per 12 s.
const REGISTER_RETRY: Duration = Duration::from_secs(13);

/// Arguments for [`run`].
#[derive(Clone)]
pub struct LoadOptions {
    /// Server address and trusted pins.
    pub target: Target,
    /// Synthetic users.
    pub bots: u32,
    /// Voice channels the bots spread over, named `load-1` … `load-M`.
    pub channels: u32,
    /// Talk, text, and churn parameters.
    pub behavior: Behavior,
    /// Disconnect and reconnect every bot together at this period.
    pub storm_every: Option<Duration>,
    /// Run length after setup.
    pub duration: Duration,
    /// Report period.
    pub interval: Duration,
    /// Delay between bot starts.
    pub ramp: Duration,
    /// Account file.
    pub state: PathBuf,
    /// Invite used for bots that have no account yet.
    pub invite: Option<String>,
    /// Username prefix. Bot `i` is `{prefix}{i:03}`.
    pub prefix: String,
    /// Encoder profile of the pre-encoded frames.
    pub profile: VoiceProfile,
    /// Per-interval client-side CSV.
    pub csv: Option<PathBuf>,
    /// Print report lines to stdout.
    pub print: bool,
}

/// Why a load run could not start.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// Bad flag value.
    #[error(transparent)]
    Plan(#[from] PlanError),
    /// Account file failure.
    #[error(transparent)]
    State(#[from] StateError),
    /// Setup session failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// Opus pre-encoding failed.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// CSV write failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A bot has no stored token and no invite was given.
    #[error("{0} has no account in the state file; pass --invite")]
    NoInvite(String),
    /// Zero bots or channels.
    #[error("--bots and --channels must be at least 1")]
    Empty,
    /// The first bot may not create the load channels.
    #[error(
        "bot {0} cannot create channels; register it with the owner invite on a fresh data dir, or create voice channels load-1..load-M and a text channel load-text"
    )]
    Channels(String),
}

/// Totals for a whole run.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// Measured time after setup.
    pub elapsed: Duration,
    /// Bots started.
    pub bots: u32,
    /// Seated sessions, including reconnects.
    pub sessions: u64,
    /// Failed connect, Hello, or join attempts.
    pub connect_failures: u64,
    /// Sessions the bots did not end.
    pub forced: u64,
    /// Why those sessions ended.
    pub forced_reasons: BTreeMap<String, u64>,
    /// Audio datagrams sent.
    pub sent: u64,
    /// Audio datagrams received.
    pub received: u64,
    /// Sequence gaps seen by receivers.
    pub lost: u64,
    /// Datagrams that arrived behind a later one.
    pub reordered: u64,
    /// `send_datagram` refusals.
    pub send_errors: u64,
    /// Relay delay median.
    pub relay_p50: Option<Duration>,
    /// Relay delay 99th percentile.
    pub relay_p99: Option<Duration>,
    /// Relay delay maximum.
    pub relay_max: Option<Duration>,
    /// Messages sent.
    pub text_sent: u64,
    /// Messages acknowledged.
    pub text_acked: u64,
    /// Messages rejected, unanswered, or cut off by a storm.
    pub text_failed: u64,
    /// Message ack median.
    pub text_p50: Option<Duration>,
    /// Message ack 99th percentile.
    pub text_p99: Option<Duration>,
    /// Connect-to-seated 99th percentile.
    pub join_p99: Option<Duration>,
    /// Voice leaves from churn.
    pub churn_leaves: u64,
    /// Storms started.
    pub storms: u64,
    /// Bots that came back after a storm.
    pub storm_rejoins: u64,
    /// Storm rejoin 95th percentile.
    pub storm_p95: Option<Duration>,
    /// Slowest storm rejoin.
    pub storm_max: Option<Duration>,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = stats::millis;
        writeln!(
            f,
            "summary: {:.0}s bots={} sessions={} connect_failures={} forced={}",
            self.elapsed.as_secs_f64(),
            self.bots,
            self.sessions,
            self.connect_failures,
            self.forced
        )?;
        for (reason, count) in &self.forced_reasons {
            writeln!(f, "  forced x{count}: {reason}")?;
        }
        writeln!(
            f,
            "audio: sent={} send_errors={} received={} gaps={} reordered={} loss={:.3}% relay p50={}ms p99={}ms max={}ms",
            self.sent,
            self.send_errors,
            self.received,
            self.lost,
            self.reordered,
            stats::loss_percent(self.received, self.lost),
            ms(self.relay_p50),
            ms(self.relay_p99),
            ms(self.relay_max),
        )?;
        writeln!(
            f,
            "text: sent={} acked={} failed={} ack p50={}ms p99={}ms; join p99={}ms; churn leaves={}",
            self.text_sent,
            self.text_acked,
            self.text_failed,
            ms(self.text_p50),
            ms(self.text_p99),
            ms(self.join_p99),
            self.churn_leaves,
        )?;
        write!(
            f,
            "storms: {} rejoins={} p95={}ms max={}ms",
            self.storms,
            self.storm_rejoins,
            ms(self.storm_p95),
            ms(self.storm_max),
        )
    }
}

/// Prepare accounts and channels, run the bots for `options.duration`, and
/// return the totals. Ctrl-C ends the run early with a summary.
pub async fn run(options: LoadOptions) -> Result<Summary, LoadError> {
    options.behavior.validate()?;
    if options.bots == 0 || options.channels == 0 {
        return Err(LoadError::Empty);
    }
    let mut accounts = State::load(&options.state)?;
    let setup = prepare(&options, &mut accounts).await?;
    let voice = Arc::new(bots::preroll(options.profile)?);
    let stats = Arc::new(Stats::default());
    let (stop_tx, stop_rx) = watch::channel(false);
    let (storm_tx, storm_rx) = watch::channel(0_u64);
    let shared = Arc::new(bot::Shared {
        target: options.target.clone(),
        behavior: options.behavior,
        voice,
        stats: Arc::clone(&stats),
        epoch: Instant::now(),
        text_channel: setup.text,
        stop: stop_rx,
        storm: storm_rx,
    });
    let mut tasks = JoinSet::new();
    for (index, (name, token)) in setup.tokens.into_iter().enumerate() {
        let slot = u32::try_from(index).unwrap_or(u32::MAX);
        let channel = setup.voice[index % setup.voice.len()];
        let bot = bot::Bot {
            rng: plan::Rng::new(u64::from(slot) + 1),
            name,
            token,
            channel,
            start_after: options.ramp * slot,
        };
        tasks.spawn(bot::run(bot, Arc::clone(&shared)));
    }
    let mut csv = match &options.csv {
        Some(path) => {
            let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
            writeln!(file, "{}", stats::CSV_HEADER)?;
            file.flush()?;
            Some(file)
        }
        None => None,
    };
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + options.duration;
    let mut report = tokio::time::interval_at(
        tokio::time::Instant::now() + options.interval,
        options.interval,
    );
    let mut next_storm = options
        .storm_every
        .map(|every| tokio::time::Instant::now() + every);
    let mut last = stats.snapshot();
    loop {
        let storm_at = next_storm.unwrap_or(deadline);
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            _ = report.tick() => {
                let now = stats.snapshot();
                if options.print {
                    println!("{}", report_line(started.elapsed(), &now, &last, options.interval));
                }
                if let Some(file) = csv.as_mut() {
                    writeln!(file, "{}", stats::csv_row(started.elapsed(), &now, &last))?;
                    file.flush()?;
                }
                last = now;
            }
            () = tokio::time::sleep_until(storm_at), if next_storm.is_some() => {
                Stats::bump(&stats.storms);
                storm_tx.send_modify(|generation| *generation += 1);
                if options.print {
                    println!("t={:.0}s reconnect storm", started.elapsed().as_secs_f64());
                }
                next_storm = options.storm_every.map(|every| storm_at + every);
            }
            result = tokio::signal::ctrl_c() => {
                let _ = result;
                break;
            }
        }
    }
    let elapsed = started.elapsed();
    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    tasks.abort_all();
    Ok(summarize(elapsed, options.bots, &stats))
}

fn summarize(elapsed: Duration, bots: u32, stats: &Stats) -> Summary {
    let end = stats.snapshot();
    Summary {
        elapsed,
        bots,
        sessions: end.sessions,
        connect_failures: end.connect_failures,
        forced: end.forced,
        forced_reasons: stats.forced_reasons(),
        sent: end.sent,
        received: end.received,
        lost: end.lost,
        reordered: end.reordered,
        send_errors: end.send_errors,
        relay_p50: end.relay.quantile(0.5),
        relay_p99: end.relay.quantile(0.99),
        relay_max: end.relay.max(),
        text_sent: end.text_sent,
        text_acked: end.text_acked,
        text_failed: end.text_failed,
        text_p50: end.text_ack.quantile(0.5),
        text_p99: end.text_ack.quantile(0.99),
        join_p99: end.join.quantile(0.99),
        churn_leaves: end.churn_leaves,
        storms: end.storms,
        storm_rejoins: end.storm_rejoin.count(),
        storm_p95: end.storm_rejoin.quantile(0.95),
        storm_max: end.storm_rejoin.max(),
    }
}

fn report_line(elapsed: Duration, now: &Snapshot, last: &Snapshot, interval: Duration) -> String {
    let seconds = interval.as_secs_f64().max(0.001);
    let rate = |now: u64, last: u64| as_f64(now.saturating_sub(last)) / seconds;
    let relay = now.relay.since(&last.relay);
    let text = now.text_ack.since(&last.text_ack);
    format!(
        "t={:.0}s online={} voice={} sent/s={:.0} recv/s={:.0} loss={:.3}% relay p50={}ms p99={}ms text={} ack p99={}ms forced={} sessions={}",
        elapsed.as_secs_f64(),
        now.online,
        now.in_voice,
        rate(now.sent, last.sent),
        rate(now.received, last.received),
        stats::loss_percent(
            now.received.saturating_sub(last.received),
            now.lost.saturating_sub(last.lost)
        ),
        stats::millis(relay.quantile(0.5)),
        stats::millis(relay.quantile(0.99)),
        now.text_acked.saturating_sub(last.text_acked),
        stats::millis(text.quantile(0.99)),
        now.forced,
        now.sessions,
    )
}

fn as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

/// Usernames for a run: `{prefix}001` …
pub fn bot_names(prefix: &str, bots: u32) -> Vec<String> {
    (1..=bots)
        .map(|index| format!("{prefix}{index:03}"))
        .collect()
}

struct Setup {
    voice: Vec<Id>,
    text: Id,
    tokens: Vec<(String, String)>,
}

/// Registers missing accounts, creates a shared invite and the load channels.
async fn prepare(options: &LoadOptions, state: &mut State) -> Result<Setup, LoadError> {
    let names = bot_names(&options.prefix, options.bots);
    let first = &names[0];
    let invite = options.invite.clone();
    let session = account(options, state, first, invite.as_deref()).await?;
    if state.invite.is_none() {
        match session
            .call(
                "CreateInvite",
                client_envelope::Msg::CreateInvite(v1::CreateInvite::default()),
            )
            .await
        {
            Ok(v1::Ack {
                result: Some(v1::ack::Result::Invite(created)),
                ..
            }) => {
                state.invite = Some(created.code);
                state.save(&options.state)?;
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "could not create a shared invite"),
        }
    }
    let (voice, text) = ensure_channels(&session, options.channels, first).await?;
    session.connection.close(0, b"setup done");
    drop(session);
    let shared_invite = state.invite.clone().or(invite);
    for name in &names[1..] {
        if !state.accounts.contains_key(name) {
            let session = account(options, state, name, shared_invite.as_deref()).await?;
            session.connection.close(0, b"registered");
        }
    }
    // Stored tokens are checked when each bot connects; a stale one shows up
    // as connect failures, and the first bot's token was refreshed above.
    let tokens = names
        .iter()
        .filter_map(|name| {
            state
                .accounts
                .get(name)
                .map(|account| (name.clone(), account.token.clone()))
        })
        .collect();
    Ok(Setup {
        voice,
        text,
        tokens,
    })
}

/// A session for `name`, registering the account first when it is new.
async fn account(
    options: &LoadOptions,
    state: &mut State,
    name: &str,
    invite: Option<&str>,
) -> Result<Session, LoadError> {
    if let Some(account) = state.accounts.get(name).cloned() {
        match session::open(&options.target, Credential::Token(&account.token)).await {
            Err(SessionError::Rejected {
                code: v1::ErrorCode::Unauthenticated,
                ..
            }) => return relogin(options, state, name, account).await,
            other => return Ok(other?),
        }
    }
    let invite = invite.ok_or_else(|| LoadError::NoInvite(name.to_owned()))?;
    let password = state::random_secret();
    loop {
        let attempt = session::open(
            &options.target,
            Credential::Register {
                invite,
                username: name,
                password: &password,
            },
        )
        .await;
        match attempt {
            Ok(session) => {
                state.accounts.insert(
                    name.to_owned(),
                    Account {
                        password,
                        token: session.token.clone(),
                    },
                );
                state.save(&options.state)?;
                if options.print {
                    eprintln!("registered {name}");
                }
                return Ok(session);
            }
            Err(error) if error.rate_limited() => {
                if options.print {
                    eprintln!(
                        "registration rate limited; retrying {name} in {}s",
                        REGISTER_RETRY.as_secs()
                    );
                }
                tokio::time::sleep(REGISTER_RETRY).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Password login after a stored token stopped working. Rate limited like
/// registration.
async fn relogin(
    options: &LoadOptions,
    state: &mut State,
    name: &str,
    account: Account,
) -> Result<Session, LoadError> {
    loop {
        let attempt = session::open(
            &options.target,
            Credential::Login {
                username: name,
                password: &account.password,
            },
        )
        .await;
        match attempt {
            Ok(session) => {
                state.accounts.insert(
                    name.to_owned(),
                    Account {
                        password: account.password,
                        token: session.token.clone(),
                    },
                );
                state.save(&options.state)?;
                return Ok(session);
            }
            Err(error) if error.rate_limited() => tokio::time::sleep(REGISTER_RETRY).await,
            Err(error) => return Err(error.into()),
        }
    }
}

/// Finds or creates `load-1` … `load-M` and [`TEXT_CHANNEL`].
async fn ensure_channels(
    session: &Session,
    count: u32,
    actor: &str,
) -> Result<(Vec<Id>, Id), LoadError> {
    let find = |name: &str, kind: v1::ChannelKind| {
        session
            .snapshot
            .channels
            .iter()
            .find(|channel| channel.name == name && channel.kind == i32::from(kind))
            .and_then(|channel| id_from_bytes(&channel.id).ok())
    };
    let mut voice = Vec::new();
    for index in 1..=count {
        let name = format!("load-{index}");
        let id = match find(&name, v1::ChannelKind::Voice) {
            Some(id) => id,
            None => create_channel(session, &name, v1::ChannelKind::Voice, index, actor).await?,
        };
        voice.push(id);
    }
    let text = match find(TEXT_CHANNEL, v1::ChannelKind::Text) {
        Some(id) => id,
        None => create_channel(session, TEXT_CHANNEL, v1::ChannelKind::Text, 0, actor).await?,
    };
    Ok((voice, text))
}

async fn create_channel(
    session: &Session,
    name: &str,
    kind: v1::ChannelKind,
    position: u32,
    actor: &str,
) -> Result<Id, LoadError> {
    let voice = (kind == v1::ChannelKind::Voice).then(|| v1::VoiceSettings {
        profile: i32::from(v1::VoiceProfile::Voice),
        ..v1::VoiceSettings::default()
    });
    let created = session
        .call(
            "CreateChannel",
            client_envelope::Msg::CreateChannel(v1::CreateChannel {
                kind: i32::from(kind),
                name: name.to_owned(),
                position: 100 + i32::try_from(position).unwrap_or(0),
                voice,
                ..v1::CreateChannel::default()
            }),
        )
        .await;
    match created {
        Ok(v1::Ack {
            result: Some(v1::ack::Result::ChannelId(id)),
            ..
        }) => id_from_bytes(&id).map_err(|_| LoadError::Channels(actor.to_owned())),
        Ok(_)
        | Err(SessionError::Rejected {
            code: v1::ErrorCode::Forbidden,
            ..
        }) => Err(LoadError::Channels(actor.to_owned())),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_names_are_valid_usernames() {
        let names = bot_names("load", 3);
        assert_eq!(names, ["load001", "load002", "load003"]);
        assert!(bot_names("load", 100).last().unwrap().ends_with("100"));
    }

    #[test]
    fn report_line_rates_use_the_interval() {
        let stats = Stats::default();
        let earlier = stats.snapshot();
        for _ in 0..500 {
            Stats::bump(&stats.sent);
        }
        let line = report_line(
            Duration::from_secs(20),
            &stats.snapshot(),
            &earlier,
            Duration::from_secs(10),
        );
        assert!(line.contains("sent/s=50 "), "{line}");
        assert!(line.starts_with("t=20s"), "{line}");
    }
}
