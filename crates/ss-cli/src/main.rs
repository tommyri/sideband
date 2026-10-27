//! `ss-cli connect` joins a voice channel on the spike server.

use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use clap::{Parser, Subcommand};
use ss_cli::{CallError, Gate, Impairment, Options, channel_from_arg, run};
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
    let Command::Connect(connect) = args.command;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(connect_command(connect))
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
