//! T-100 LiveKit spike. See README.md. Every role prints JSON lines to stdout.

mod baseline;
mod impair;
#[cfg(feature = "livekit")]
mod lk;
mod metrics;
mod pattern;
mod pixels;
mod source;
mod stamp;
#[cfg(feature = "livekit")]
mod turn_relay;

use std::{net::SocketAddr, path::PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum PresetArg {
    P1080p60,
    P1080p30,
    P720p30,
}

impl From<PresetArg> for ss_video::Preset {
    fn from(p: PresetArg) -> Self {
        match p {
            PresetArg::P1080p60 => Self::FullHd60,
            PresetArg::P1080p30 => Self::FullHd30,
            PresetArg::P720p30 => Self::Hd30,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// List capturable displays (ids for --content screen:<id>).
    Sources,
    /// Baseline: the real ss-server relay on 127.0.0.1, users pre-registered.
    BaselineServer {
        #[arg(long)]
        run_dir: Option<PathBuf>,
    },
    /// Baseline publisher: frames -> ss_video encoder settings -> GOP streams.
    BaselinePub {
        #[arg(long)]
        run_dir: Option<PathBuf>,
        #[arg(long, default_value = "pattern")]
        content: source::Content,
        #[arg(long, value_enum, default_value = "p1080p30")]
        preset: PresetArg,
        #[arg(long, default_value_t = 60)]
        seconds: u64,
    },
    /// Baseline viewer: subscribe, decode with ss_video, read barcodes.
    BaselineView {
        #[arg(long)]
        run_dir: Option<PathBuf>,
        #[arg(long, default_value = "viewer1")]
        name: String,
        /// Connect through this local port (the impairment proxy).
        #[arg(long)]
        via: Option<u16>,
        #[arg(long, default_value_t = 60)]
        seconds: u64,
        #[arg(long)]
        leave_at: Option<u64>,
        #[arg(long)]
        rejoin_at: Option<u64>,
        #[arg(long)]
        reconnect_at: Option<u64>,
    },
    #[cfg(feature = "livekit")]
    /// LiveKit publisher: frames -> NativeVideoSource (H.264, simulcast).
    LkPub {
        #[arg(long, default_value = "t100")]
        room: String,
        #[arg(long, default_value = "pattern")]
        content: source::Content,
        #[arg(long, value_enum, default_value = "p1080p30")]
        preset: PresetArg,
        #[arg(long, default_value_t = 60)]
        seconds: u64,
        #[arg(long)]
        no_simulcast: bool,
        /// Let libwebrtc choose the encoder instead of asking for VideoToolbox.
        #[arg(long)]
        auto_encoder: bool,
        /// Top-layer bitrate; defaults to the preset's (the baseline's) bitrate.
        #[arg(long)]
        bitrate: Option<u64>,
    },
    #[cfg(feature = "livekit")]
    /// LiveKit viewer.
    LkView {
        #[arg(long, default_value = "t100")]
        room: String,
        #[arg(long, default_value = "viewer1")]
        name: String,
        /// Relay-only through TURN on this local port (the impairment proxy).
        #[arg(long)]
        turn: Option<u16>,
        #[arg(long, default_value_t = 60)]
        seconds: u64,
        #[arg(long)]
        signal_reconnect_at: Option<u64>,
        #[arg(long)]
        full_reconnect_at: Option<u64>,
        #[arg(long)]
        revoke_at: Option<u64>,
        #[arg(long)]
        restore_at: Option<u64>,
        #[arg(long)]
        move_at: Option<u64>,
        #[arg(long)]
        move_back_at: Option<u64>,
        #[arg(long)]
        kick_at: Option<u64>,
        #[arg(long)]
        ttl: Option<u64>,
    },
    #[cfg(feature = "livekit")]
    /// Check the local LiveKit server answers.
    LkPing,
    #[cfg(feature = "livekit")]
    /// Create a LiveKit room, optionally with a subscriber playout-delay bound.
    LkRoom {
        #[arg(long)]
        name: String,
        #[arg(long)]
        min_playout_ms: Option<u32>,
        #[arg(long)]
        max_playout_ms: Option<u32>,
    },
    #[cfg(feature = "livekit")]
    /// TURN server for the relay-only viewer.
    Turn {
        #[arg(long, default_value = "127.0.0.1:3479")]
        listen: SocketAddr,
    },
    /// UDP impairment proxy.
    Impair {
        #[arg(long)]
        listen: SocketAddr,
        #[arg(long)]
        upstream: SocketAddr,
        /// e.g. `20:clean,30:1500kbps/2%/40ms,40:clean`
        #[arg(long)]
        schedule: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Sources => {
            for s in ss_video::list_sources().await? {
                println!(
                    "{:?}\t{}\t{}x{}\t{}",
                    s.kind, s.id, s.width, s.height, s.label
                );
            }
        }
        Command::BaselineServer { run_dir } => {
            baseline::serve(&baseline::run_dir(&run_dir)).await?
        }
        Command::BaselinePub {
            run_dir,
            content,
            preset,
            seconds,
        } => {
            baseline::publish(
                &baseline::run_dir(&run_dir),
                &content,
                preset.into(),
                seconds,
            )
            .await?;
        }
        Command::BaselineView {
            run_dir,
            name,
            via,
            seconds,
            leave_at,
            rejoin_at,
            reconnect_at,
        } => {
            let life = baseline::Lifecycle {
                leave_at,
                rejoin_at,
                reconnect_at,
            };
            baseline::view(&baseline::run_dir(&run_dir), &name, via, seconds, life).await?;
        }
        #[cfg(feature = "livekit")]
        Command::LkPub {
            room,
            content,
            preset,
            seconds,
            no_simulcast,
            auto_encoder,
            bitrate,
        } => {
            let preset: ss_video::Preset = preset.into();
            let settings = lk::PublishSettings {
                simulcast: !no_simulcast,
                hardware: !auto_encoder,
                bitrate: bitrate.unwrap_or(preset.bitrate()),
            };
            lk::publish(&room, &content, preset, settings, seconds).await?;
        }
        #[cfg(feature = "livekit")]
        Command::LkView {
            room,
            name,
            turn,
            seconds,
            signal_reconnect_at,
            full_reconnect_at,
            revoke_at,
            restore_at,
            move_at,
            move_back_at,
            kick_at,
            ttl,
        } => {
            let life = lk::Lifecycle {
                signal_reconnect_at,
                full_reconnect_at,
                revoke_at,
                restore_at,
                move_at,
                move_back_at,
                kick_at,
                ttl,
            };
            lk::view(&room, &name, turn, seconds, life).await?;
        }
        #[cfg(feature = "livekit")]
        Command::LkPing => lk::ping().await?,
        #[cfg(feature = "livekit")]
        Command::LkRoom {
            name,
            min_playout_ms,
            max_playout_ms,
        } => {
            let playout = max_playout_ms.map(|max| (min_playout_ms.unwrap_or(0), max));
            lk::create_room(&name, playout).await?;
        }
        #[cfg(feature = "livekit")]
        Command::Turn { listen } => turn_relay::run(listen).await?,
        Command::Impair {
            listen,
            upstream,
            schedule,
        } => {
            impair::run(listen, upstream, impair::parse_schedule(&schedule)?).await?;
        }
    }
    Ok(())
}
