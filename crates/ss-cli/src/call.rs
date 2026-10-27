//! Connect, join, and run one or two in-process clients against a spike server.

use std::{
    io::IsTerminal,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use bytes::Bytes;
use ss_core::{Id, VoiceProfile};
use ss_net::{
    Connection,
    control::{ClientControl, ControlError, Reply},
    pins::PinStore,
};
use ss_proto::{
    v1::{self, client_envelope, server_envelope},
    validate::id_to_bytes,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::timeout,
};

use crate::{
    audio::{self, AudioConfig, Incoming, LiveStats, TrackUpdate},
    bots,
    graph::{GraphError, VoiceGraph, packet_seq},
    impair::{Impairment, Scheduler},
    measure::{self, Probe, median},
    terminal,
};

/// Why a call did not start or stay up.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// QUIC, TLS, or pin store failure.
    #[error(transparent)]
    Transport(#[from] ss_net::Error),
    /// The control stream failed.
    #[error(transparent)]
    Control(#[from] ControlError),
    /// The local encoder rejected a frame.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// A capture or playback device could not be opened.
    #[error(transparent)]
    Device(#[from] ss_audio::device::Error),
    /// Filesystem failure while recording.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The server returned `Error` for a command.
    #[error("server rejected {command}: {message}")]
    Rejected {
        /// Command that failed.
        command: &'static str,
        /// Server-provided text.
        message: String,
    },
    /// Join completed without a `TrackAdded` for this client.
    #[error("the server closed the control stream before assigning a track")]
    NoTrack,
    /// Hello or join exceeded its deadline.
    #[error("timed out waiting for the server")]
    Timeout,
    /// The voice thread exited before reporting its devices.
    #[error("voice thread stopped before the call started")]
    AudioStopped,
    /// Push-to-talk needs a terminal. Use `--open-mic` otherwise.
    #[error("push-to-talk needs a terminal; pass --open-mic or --vad")]
    NoTerminal,
}

/// How captured audio becomes a transmit gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Spacebar, held or toggled depending on the terminal.
    PushToTalk,
    /// Transmit every captured frame.
    Open,
    /// Transmit while the frame energy stays above the gate.
    Vad,
}

/// Arguments for [`run`], after the pin prompt.
#[derive(Clone)]
pub struct Options {
    /// Server hostname or IP, without a port.
    pub host: String,
    /// UDP port.
    pub port: u16,
    /// Username sent in `Hello`. The spike server does not check the password.
    pub name: String,
    /// Voice channel.
    pub channel: Id,
    /// Encoder profile. Both people in a call pass the same one.
    pub profile: VoiceProfile,
    /// Transmit gate.
    pub gate: Gate,
    /// Run a second in-process client and print mouth-to-ear latency.
    pub measure_latency: bool,
    /// Outgoing datagram impairment. Bots are not impaired.
    pub impairment: Impairment,
    /// Extra synthetic senders in this process.
    pub bots: u32,
    /// WAV path for the mixed playback.
    pub record: Option<PathBuf>,
    /// Certificate pins. The caller approves an unknown certificate first.
    pub pins: Arc<dyn PinStore>,
    /// Stop after this long. `None` runs until Ctrl-C or q.
    pub duration: Option<Duration>,
}

/// Click totals from a measurement run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// One sample per correlated click.
    pub samples: Vec<Duration>,
    /// Median of [`Self::samples`].
    pub median: Option<Duration>,
}

pub(crate) struct Joined {
    pub(crate) connection: Connection,
    pub(crate) control: ClientControl,
    pub(crate) events: mpsc::Receiver<v1::ServerEnvelope>,
    pub(crate) track_id: u32,
    pub(crate) user_id: Bytes,
    pub(crate) remote_tracks: Vec<u32>,
}

/// Join `name` with the host, channel, and pins in `options`.
pub(crate) async fn join_client(options: &Options, name: &str) -> Result<Joined, CallError> {
    let connection =
        ss_net::client::connect(&options.host, options.port, Arc::clone(&options.pins)).await?;
    let mut connection = connection;
    let stream = connection.open_control().await?;
    let (control, mut events) = ClientControl::start(stream);
    let response = timeout(
        Duration::from_secs(10),
        control.request(client_envelope::Msg::Hello(v1::Hello {
            protocol_version: ss_proto::PROTOCOL_VERSION,
            client_name: "ss-cli".to_owned(),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            auth: Some(v1::hello::Auth::Login(v1::Login {
                username: name.to_owned(),
                password: String::new(),
            })),
        })),
    )
    .await
    .map_err(|_| CallError::Timeout)??;
    let user_id = match response.msg {
        Some(server_envelope::Msg::Welcome(welcome)) => {
            welcome
                .user
                .ok_or(CallError::Rejected {
                    command: "Hello",
                    message: "Welcome omitted the user".to_owned(),
                })?
                .id
        }
        Some(server_envelope::Msg::Error(error)) => {
            return Err(CallError::Rejected {
                command: "Hello",
                message: error.message,
            });
        }
        _ => {
            return Err(CallError::Rejected {
                command: "Hello",
                message: "unexpected reply".to_owned(),
            });
        }
    };
    let reply = timeout(
        Duration::from_secs(10),
        control.call(client_envelope::Msg::JoinVoice(v1::JoinVoice {
            channel_id: id_to_bytes(options.channel),
            self_mute: false,
            self_deaf: false,
        })),
    )
    .await
    .map_err(|_| CallError::Timeout)??;
    if let Reply::Error(error) = reply {
        return Err(CallError::Rejected {
            command: "JoinVoice",
            message: error.message,
        });
    }
    let (track_id, remote_tracks) = collect_tracks(&mut events, &user_id).await?;
    tracing::info!(%name, track_id, "joined voice");
    Ok(Joined {
        connection,
        control,
        events,
        track_id,
        user_id,
        remote_tracks,
    })
}

async fn collect_tracks(
    events: &mut mpsc::Receiver<v1::ServerEnvelope>,
    user_id: &Bytes,
) -> Result<(u32, Vec<u32>), CallError> {
    let mut track_id = 0_u32;
    let mut remote_tracks = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while track_id == 0 && Instant::now() < deadline {
        let Ok(Some(event)) = timeout(Duration::from_millis(200), events.recv()).await else {
            continue;
        };
        note_track(&event, user_id, &mut track_id, &mut remote_tracks);
    }
    while let Ok(event) = events.try_recv() {
        note_track(&event, user_id, &mut track_id, &mut remote_tracks);
    }
    if track_id == 0 {
        return Err(CallError::NoTrack);
    }
    Ok((track_id, remote_tracks))
}

fn note_track(
    event: &v1::ServerEnvelope,
    user_id: &Bytes,
    track_id: &mut u32,
    remote_tracks: &mut Vec<u32>,
) {
    match &event.msg {
        Some(server_envelope::Msg::TrackAdded(track)) if track.user_id == *user_id => {
            *track_id = track.track_id;
        }
        Some(server_envelope::Msg::TrackAdded(track)) if track.track_id != 0 => {
            remote_tracks.push(track.track_id);
        }
        Some(server_envelope::Msg::TrackRemoved(track)) => {
            remote_tracks.retain(|id| *id != track.track_id);
        }
        _ => {}
    }
}

/// Run the call described by `options`.
#[allow(clippy::too_many_lines)]
pub async fn run(options: Options) -> Result<Report, CallError> {
    if options.gate == Gate::PushToTalk && !std::io::stdin().is_terminal() {
        return Err(CallError::NoTerminal);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (stop_tx, stop_rx) = watch::channel(false);
    let probe = Arc::new(Probe::new());
    let epoch = Instant::now();
    let bots = bots::spawn(&options, &stop_rx);
    let talker = join_client(&options, &options.name).await?;
    eprintln!(
        "joined channel {} as {} (track {})",
        options.channel, options.name, talker.track_id
    );
    let ear = if options.measure_latency {
        let name = format!("{}-ear", options.name);
        Some(join_client(&options, &name).await?)
    } else {
        None
    };
    let stats = Arc::new(std::sync::Mutex::new(LiveStats::default()));
    let jitter_us = Arc::new(AtomicU64::new(0));
    let (log_tx, log_rx) = mpsc::unbounded_channel();
    let talking = Arc::new(AtomicBool::new(options.gate == Gate::Open));
    let mut tasks = JoinSet::new();
    let talker_connection = Arc::new(talker.connection);
    let audio = Box::pin(launch_audio(
        &options,
        talker.track_id,
        &talker.remote_tracks,
        Arc::clone(&talker_connection),
        talker.events,
        &talker.user_id,
        AudioSide {
            measure: options.measure_latency,
            correlate: false,
            capture: true,
            devices_required: !options.measure_latency,
            record: if options.measure_latency {
                None
            } else {
                options.record.clone()
            },
        },
        Arc::clone(&probe),
        epoch,
        Arc::clone(&stop),
        stop_rx.clone(),
        Arc::clone(&talking),
        Arc::clone(&stats),
        Arc::clone(&jitter_us),
        log_tx.clone(),
        &mut tasks,
    ))
    .await?;
    let (ear_audio, ear_control) = match ear {
        Some(ear) => {
            let control = ear.control;
            let connection = Arc::new(ear.connection);
            let handle = Box::pin(launch_audio(
                &options,
                ear.track_id,
                &ear.remote_tracks,
                connection,
                ear.events,
                &ear.user_id,
                AudioSide {
                    measure: false,
                    correlate: true,
                    capture: false,
                    devices_required: false,
                    record: options.record.clone(),
                },
                Arc::clone(&probe),
                epoch,
                Arc::clone(&stop),
                stop_rx.clone(),
                Arc::new(AtomicBool::new(false)),
                Arc::new(std::sync::Mutex::new(LiveStats::default())),
                Arc::new(AtomicU64::new(0)),
                log_tx.clone(),
                &mut tasks,
            ))
            .await?;
            (Some(handle), Some(control))
        }
        None => (None, None),
    };
    // Dropping a control handle closes that QUIC connection. Hold both until
    // the audio threads have been joined.
    let _controls = (talker.control, ear_control);
    let keys = if options.gate == Gate::PushToTalk {
        Some(terminal::spawn(Arc::clone(&talking), Arc::clone(&stop))?)
    } else {
        match options.gate {
            Gate::Open => eprintln!("microphone is open; Ctrl-C quits"),
            Gate::Vad => eprintln!("voice activation is on; Ctrl-C quits"),
            Gate::PushToTalk => {}
        }
        None
    };
    let stats_stop = stop_rx.clone();
    let stats_connection = Arc::clone(&talker_connection);
    tasks.spawn(async move {
        print_stats(
            stats_connection,
            stats,
            jitter_us,
            log_rx,
            stats_stop,
            Instant::now(),
        )
        .await;
    });
    wait(&options, &stop).await;
    stop.store(true, Ordering::Relaxed);
    let _ = stop_tx.send(true);
    let _ = audio.join();
    if let Some(ear_audio) = ear_audio {
        let _ = ear_audio.join();
    }
    if let Some(keys) = keys {
        let _ = keys.join();
    }
    tasks.abort_all();
    for bot in bots {
        bot.abort();
    }
    let samples = probe.totals();
    let median = median(&samples);
    if options.measure_latency {
        match median {
            Some(median) => println!(
                "median mouth-to-ear {} ms over {} clicks",
                measure::millis(median),
                samples.len()
            ),
            None => println!("mouth-to-ear measurement produced no correlated clicks"),
        }
    }
    Ok(Report { samples, median })
}

#[allow(clippy::struct_excessive_bools)]
struct AudioSide {
    measure: bool,
    correlate: bool,
    capture: bool,
    devices_required: bool,
    record: Option<PathBuf>,
}

#[allow(clippy::too_many_arguments)]
async fn launch_audio(
    options: &Options,
    track_id: u32,
    remote_tracks: &[u32],
    connection: Arc<Connection>,
    events: mpsc::Receiver<v1::ServerEnvelope>,
    user_id: &Bytes,
    side: AudioSide,
    probe: Arc<Probe>,
    epoch: Instant,
    stop: Arc<AtomicBool>,
    stop_rx: watch::Receiver<bool>,
    talking: Arc<AtomicBool>,
    stats: Arc<std::sync::Mutex<LiveStats>>,
    jitter_us: Arc<AtomicU64>,
    log: mpsc::UnboundedSender<String>,
    tasks: &mut JoinSet<()>,
) -> Result<JoinHandle<()>, CallError> {
    let mut graph = VoiceGraph::new(options.profile, track_id)?;
    for id in remote_tracks {
        graph.add_track(*id);
    }
    let (incoming_tx, incoming_rx) = mpsc::channel(256);
    let (track_tx, track_rx) = mpsc::channel(64);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(128);
    let user_id = user_id.clone();
    let mut events_stop = stop_rx.clone();
    let channels = u32::from(options.profile.encoder_params().channels);
    tasks.spawn(async move {
        let mut events = events;
        loop {
            tokio::select! {
                changed = events_stop.changed() => {
                    if changed.is_err() || *events_stop.borrow() {
                        break;
                    }
                }
                event = events.recv() => {
                    let Some(event) = event else { break };
                    if let Some(update) = track_update(&event, &user_id, channels)
                        && track_tx.send(update).await.is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    let recv_connection = Arc::clone(&connection);
    let recv_stop = stop_rx.clone();
    let expected = options.profile.encoder_params().frame_size;
    tasks.spawn(async move {
        receive_loop(
            recv_connection,
            incoming_tx,
            epoch,
            jitter_us,
            expected,
            recv_stop,
        )
        .await;
    });
    let send_connection = Arc::clone(&connection);
    let send_stop = stop_rx;
    let impairment = options.impairment;
    tasks.spawn(async move {
        send_loop(send_connection, outgoing_rx, impairment, send_stop).await;
    });
    audio::start(AudioConfig {
        graph,
        gate: options.gate,
        talking,
        measure: side.measure,
        correlate: side.correlate,
        capture: side.capture,
        devices_required: side.devices_required,
        probe,
        record: side.record,
        epoch,
        stop,
        incoming: incoming_rx,
        tracks: track_rx,
        outgoing: outgoing_tx,
        stats,
        log,
    })
    .await
}

fn track_update(event: &v1::ServerEnvelope, user_id: &Bytes, channels: u32) -> Option<TrackUpdate> {
    match &event.msg {
        Some(server_envelope::Msg::TrackAdded(track)) if track.user_id == *user_id => None,
        Some(server_envelope::Msg::TrackAdded(track)) => {
            if track
                .codec
                .as_ref()
                .is_some_and(|codec| codec.channels != channels)
            {
                tracing::warn!(
                    advertised = track.codec.as_ref().map(|codec| codec.channels),
                    local = channels,
                    "spike server codec channels differ from --profile; both clients must match"
                );
            }
            Some(TrackUpdate::Add(track.track_id))
        }
        Some(server_envelope::Msg::TrackRemoved(track)) => {
            Some(TrackUpdate::Remove(track.track_id))
        }
        _ => None,
    }
}

async fn receive_loop(
    connection: Arc<Connection>,
    incoming: mpsc::Sender<Incoming>,
    epoch: Instant,
    jitter_us: Arc<AtomicU64>,
    expected: Duration,
    mut stop: watch::Receiver<bool>,
) {
    let mut last = None;
    let mut ewma = 0.0_f64;
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            datagram = connection.read_datagram() => {
                let Ok(bytes) = datagram else { break };
                observe_gap(&mut last, &mut ewma, expected, &jitter_us);
                let _ = incoming.try_send(Incoming { bytes, at: epoch.elapsed() });
            }
        }
    }
}

fn observe_gap(
    last: &mut Option<Instant>,
    ewma: &mut f64,
    expected: Duration,
    jitter_us: &AtomicU64,
) {
    let now = Instant::now();
    if let Some(previous) = *last {
        let gap_ms = now.saturating_duration_since(previous).as_secs_f64() * 1_000.0;
        let expected_ms = expected.as_secs_f64() * 1_000.0;
        let deviation = (gap_ms - expected_ms).abs();
        *ewma = if *ewma == 0.0 {
            deviation
        } else {
            0.9 * *ewma + 0.1 * deviation
        };
        if ewma.is_finite() && *ewma >= 0.0 {
            jitter_us.store(f64_micros(*ewma), Ordering::Relaxed);
        }
    }
    *last = Some(now);
}

fn f64_micros(millis: f64) -> u64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (millis * 1_000.0) as u64
    }
}

async fn send_loop(
    connection: Arc<Connection>,
    mut packets: mpsc::Receiver<Bytes>,
    impairment: Impairment,
    mut stop: watch::Receiver<bool>,
) {
    let mut scheduler = Scheduler::new(impairment);
    loop {
        let waiting = scheduler.next_deadline().is_some();
        let wait = scheduler
            .next_deadline()
            .map_or(Duration::from_secs(1), |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            packet = packets.recv() => {
                let Some(packet) = packet else { break };
                match packet_seq(&packet) {
                    Some(seq) => scheduler.enqueue(seq, packet, Instant::now()),
                    None => {
                        if connection.send_datagram(packet).is_err() {
                            break;
                        }
                    }
                }
            }
            () = tokio::time::sleep(wait), if waiting => {}
        }
        for packet in scheduler.drain_due(Instant::now()) {
            if connection.send_datagram(packet).is_err() {
                return;
            }
        }
    }
}

async fn print_stats(
    connection: Arc<Connection>,
    stats: Arc<std::sync::Mutex<LiveStats>>,
    jitter_us: Arc<AtomicU64>,
    mut lines: mpsc::UnboundedReceiver<String>,
    mut stop: watch::Receiver<bool>,
    started: Instant,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut last_bytes = 0_u64;
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            line = lines.recv() => {
                let Some(line) = line else { break };
                println!("{line}");
            }
            _ = tick.tick() => {
                let live = *stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let path = connection.stats();
                let delta = live.sent_bytes.saturating_sub(last_bytes);
                last_bytes = live.sent_bytes;
                let bitrate = f64_from_u64(delta.saturating_mul(8)) / 1_000.0;
                let total = live.accepted.saturating_add(live.lost);
                let loss = live.lost.saturating_mul(100).checked_div(total).unwrap_or(0);
                println!(
                    "t={:.0}s rtt={} ms loss={loss}% jitter={} ms buffer={} ms underruns={} overruns={} bitrate={bitrate:.0}kbps speakers={} plc={}",
                    started.elapsed().as_secs_f64(),
                    measure::millis(path.rtt),
                    measure::millis(Duration::from_micros(jitter_us.load(Ordering::Relaxed))),
                    measure::millis(live.buffer),
                    live.underruns,
                    live.overruns,
                    live.speakers,
                    live.plc,
                );
            }
        }
    }
}

fn f64_from_u64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

async fn wait(options: &Options, stop: &AtomicBool) {
    let started = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if options
            .duration
            .is_some_and(|limit| started.elapsed() >= limit)
        {
            break;
        }
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                let _ = result;
                break;
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}
