//! `ss-cli connect` joins a voice channel on the spike server. `ss-cli load`
//! drives synthetic users against an authenticated server.

use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use clap::{Parser, Subcommand};
use ss_cli::{
    Behavior, CallError, Gate, Impairment, LoadOptions, Options, Target, channel_from_arg, run,
    run_load,
};
use ss_core::VoiceProfile;
use ss_net::{
    pins::{JsonPinStore, PinStore, ServerId},
    tls::Fingerprint,
};

#[derive(Parser)]
#[command(version, about = "Headless SocialSpeak client")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Join a voice channel and run the audio graph.
    Connect(ConnectArgs),
    /// Drive synthetic users against `ss-server run` for load and soak tests.
    Load(LoadArgs),
}

#[derive(clap::Args)]
struct LoadArgs {
    /// `host:port` of the authenticated QUIC listener.
    address: String,
    /// Synthetic users.
    #[arg(long, default_value_t = 25)]
    bots: u32,
    /// Voice channels the bots spread over, `load-1` … `load-M`.
    #[arg(long, default_value_t = 3)]
    channels: u32,
    /// Fraction of time each bot talks, 0 to 1.
    #[arg(long, default_value_t = 0.3)]
    duty: f64,
    /// Mean talk-burst length in seconds.
    #[arg(long, default_value_t = 3.0)]
    burst: f64,
    /// Text messages per bot per minute. 0 sends none.
    #[arg(long, default_value_t = 1.0)]
    text_rate: f64,
    /// Mean seconds in voice before a bot leaves and rejoins. 0 disables churn.
    #[arg(long, default_value_t = 0)]
    churn: u64,
    /// Mean seconds out of voice after a churn leave.
    #[arg(long, default_value_t = 5)]
    away: u64,
    /// Every N seconds all bots disconnect and reconnect at once. 0 disables.
    #[arg(long, default_value_t = 0)]
    storm_every: u64,
    /// Measured run length in seconds, after accounts and channels exist.
    #[arg(long, default_value_t = 60)]
    duration: u64,
    /// Seconds between report lines.
    #[arg(long, default_value_t = 10)]
    interval: u64,
    /// Milliseconds between bot starts.
    #[arg(long, default_value_t = 50)]
    ramp_ms: u64,
    /// Account and token file, reused across runs.
    #[arg(long, default_value = "ss-load-state.txt")]
    state: PathBuf,
    /// Invite for bots without an account. The owner invite on a fresh server
    /// lets the first bot create the channels and a shared invite.
    #[arg(long)]
    invite: Option<String>,
    /// Username prefix.
    #[arg(long, default_value = "load")]
    prefix: String,
    /// Encoder profile of the pre-encoded frames.
    #[arg(long, default_value = "voice")]
    profile: String,
    /// Per-interval client-side CSV.
    #[arg(long)]
    csv: Option<PathBuf>,
    /// TOFU pin file.
    #[arg(long, default_value = "socialspeak-pins.json")]
    pin_file: PathBuf,
    /// Accept an unpinned certificate without a prompt.
    #[arg(long)]
    accept_pin: bool,
}

#[derive(clap::Args)]
#[allow(clippy::struct_excessive_bools)]
struct ConnectArgs {
    /// `host:port` of the spike server.
    address: String,
    /// Username. The spike server does not check the password.
    #[arg(long)]
    name: String,
    /// Voice channel. A decimal integer or a ULID. Integers are stable across processes.
    #[arg(long)]
    channel: String,
    /// `voice`, `music`, or `low_latency`. Both clients in a call must match.
    #[arg(long, default_value = "voice")]
    profile: String,
    /// Transmit while the microphone is above the energy gate.
    #[arg(long)]
    vad: bool,
    /// Transmit every captured frame.
    #[arg(long)]
    open_mic: bool,
    /// Send clicks and print mouth-to-ear latency from a second in-process client.
    #[arg(long)]
    measure_latency: bool,
    /// `loss=5,jitter=30,reorder=1` applied to outgoing datagrams. Percents and milliseconds.
    #[arg(long)]
    impair: Option<String>,
    /// Extra synthetic senders joined to the same channel.
    #[arg(long, default_value_t = 0)]
    bots: u32,
    /// Write the mixed playback to a 48 kHz stereo WAV.
    #[arg(long)]
    record: Option<PathBuf>,
    /// TOFU pin file.
    #[arg(long, default_value = "socialspeak-pins.json")]
    pin_file: PathBuf,
    /// Accept an unpinned certificate without a prompt.
    #[arg(long)]
    accept_pin: bool,
    /// Exit after this many seconds. Omit to run until Ctrl-C or q.
    #[arg(long)]
    duration: Option<u64>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    match args.command {
        Command::Connect(connect) => runtime.block_on(connect_command(connect)),
        Command::Load(load) => runtime.block_on(load_command(load)),
    }
}

async fn load_command(args: LoadArgs) -> anyhow::Result<()> {
    let (host, port) = split_host_port(&args.address)?;
    let profile = args
        .profile
        .parse::<VoiceProfile>()
        .map_err(|error| anyhow::anyhow!(error))?;
    let burst = Duration::try_from_secs_f64(args.burst)
        .map_err(|_| anyhow::anyhow!("--burst must be a positive number of seconds"))?;
    let pins = Arc::new(JsonPinStore::open(&args.pin_file)?);
    trust_server(&host, port, &pins, args.accept_pin).await?;
    let seconds = |value: u64| (value > 0).then(|| Duration::from_secs(value));
    let options = LoadOptions {
        target: Target { host, port, pins },
        bots: args.bots,
        channels: args.channels,
        behavior: Behavior {
            duty: args.duty,
            burst,
            text_per_minute: args.text_rate,
            churn: seconds(args.churn),
            away: Duration::from_secs(args.away),
        },
        storm_every: seconds(args.storm_every),
        duration: Duration::from_secs(args.duration),
        interval: Duration::from_secs(args.interval.max(1)),
        ramp: Duration::from_millis(args.ramp_ms),
        state: args.state,
        invite: args.invite,
        prefix: args.prefix,
        profile,
        csv: args.csv,
        print: true,
    };
    let summary = run_load(options).await?;
    println!("{summary}");
    if summary.forced > 0 {
        anyhow::bail!(
            "{} sessions were closed by the server or the path",
            summary.forced
        );
    }
    Ok(())
}

/// Pins the server on first use when `--accept-pin` is set.
async fn trust_server(
    host: &str,
    port: u16,
    pins: &Arc<JsonPinStore>,
    accept_pin: bool,
) -> anyhow::Result<()> {
    let store: Arc<dyn PinStore> = pins.clone();
    match ss_net::client::connect(host, port, store).await {
        Ok(connection) => {
            connection.close(0, b"pin check");
            Ok(())
        }
        Err(ss_net::Error::Untrusted { fingerprint }) if accept_pin => {
            pins.set(&ServerId::new(host, port)?, fingerprint)?;
            eprintln!("pinned {fingerprint}");
            Ok(())
        }
        Err(ss_net::Error::Untrusted { fingerprint }) => anyhow::bail!(
            "untrusted server certificate {fingerprint}; rerun with --accept-pin to store it"
        ),
        Err(error) => Err(error.into()),
    }
}

async fn connect_command(args: ConnectArgs) -> anyhow::Result<()> {
    if args.vad && args.open_mic {
        anyhow::bail!("pass only one of --vad and --open-mic");
    }
    let (host, port) = split_host_port(&args.address)?;
    let profile = args
        .profile
        .parse::<VoiceProfile>()
        .map_err(|error| anyhow::anyhow!(error))?;
    let channel = channel_from_arg(&args.channel)?;
    let impairment = Impairment::parse(args.impair.as_deref().unwrap_or(""))?;
    let gate = if args.open_mic {
        Gate::Open
    } else if args.vad {
        Gate::Vad
    } else {
        Gate::PushToTalk
    };
    let pins = Arc::new(JsonPinStore::open(&args.pin_file)?);
    let mut options = Options {
        host,
        port,
        name: args.name.clone(),
        channel,
        profile,
        gate,
        measure_latency: args.measure_latency,
        impairment,
        bots: args.bots,
        record: args.record.clone(),
        pins: pins.clone(),
        duration: args.duration.map(Duration::from_secs),
    };
    match run(options.clone()).await {
        Err(CallError::Transport(ss_net::Error::Untrusted { fingerprint })) => {
            approve(&args, &pins, &options.host, options.port, fingerprint)?;
            options.pins = pins;
            run(options).await?;
        }
        Err(CallError::Transport(ss_net::Error::FingerprintChanged { expected, received })) => {
            anyhow::bail!(
                "server fingerprint changed from {expected} to {received}; remove its line in {} to review it",
                args.pin_file.display()
            );
        }
        result => {
            result?;
        }
    }
    Ok(())
}

fn approve(
    args: &ConnectArgs,
    pins: &JsonPinStore,
    host: &str,
    port: u16,
    fingerprint: Fingerprint,
) -> anyhow::Result<()> {
    let trusted = if args.accept_pin {
        true
    } else if io::stdin().is_terminal() {
        prompt(&fingerprint)?
    } else {
        anyhow::bail!(
            "untrusted server certificate {fingerprint}; rerun with --accept-pin to store it"
        );
    };
    if !trusted {
        anyhow::bail!("server certificate was not trusted");
    }
    let server = ServerId::new(host, port)?;
    pins.set(&server, fingerprint)?;
    eprintln!("pinned {fingerprint}");
    Ok(())
}

fn prompt(fingerprint: &Fingerprint) -> anyhow::Result<bool> {
    eprintln!("The server certificate is not pinned.");
    eprintln!("SHA-256 {fingerprint}");
    eprint!("Trust this server and continue? [y/N] ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let answer = line.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

fn split_host_port(address: &str) -> anyhow::Result<(String, u16)> {
    if let Some(rest) = address.strip_prefix('[') {
        let (host, port) = rest
            .split_once("]:")
            .ok_or_else(|| anyhow::anyhow!("address must be host:port"))?;
        return Ok((host.to_owned(), parse_port(port)?));
    }
    let (host, port) = address
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("address must be host:port"))?;
    if host.is_empty() {
        anyhow::bail!("address must be host:port");
    }
    Ok((host.to_owned(), parse_port(port)?))
}

fn parse_port(port: &str) -> anyhow::Result<u16> {
    port.parse()
        .map_err(|_| anyhow::anyhow!("port must be a number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_host_and_bracketed_ipv6() {
        assert_eq!(
            split_host_port("127.0.0.1:443").unwrap(),
            ("127.0.0.1".to_owned(), 443)
        );
        assert_eq!(split_host_port("[::1]:9").unwrap(), ("::1".to_owned(), 9));
    }
}
