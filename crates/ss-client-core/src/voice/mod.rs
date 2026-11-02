//! Voice-session orchestration for headless and desktop clients.

mod graph;
mod worker;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use parking_lot::RwLock;
use ss_audio::{device::DeviceId, processing::ProcessingConfig, vad};
use ss_core::{Id, VoiceProfile};
use ss_proto::{v1, validate::id_to_bytes};
use tokio::{
    sync::{broadcast, oneshot},
    task::JoinHandle,
};

use crate::{
    Client, ClientEvent, ConnectionState, Error, StateStore,
    connection::{Command, TransportStats},
    state::id,
    storage::Storage,
};

use self::{
    graph::TrackSpec,
    worker::{Update, WorkerHandle},
};

/// Where capture samples come from. Test and silent sources avoid hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioSource {
    /// Open the configured or default system devices.
    #[default]
    Device,
    /// Deterministic 440 Hz tone for integration tests.
    Test,
    /// Clocked silence for a receiving-only test client.
    Silent,
}

/// System device backend used for a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeviceBackend {
    /// Portable cpal capture/playback plus the optional T-053 software chain.
    #[default]
    Portable,
    /// Apple's full-duplex VoiceProcessingIO AEC/NS/AGC path.
    ///
    /// This follows the macOS system-default devices and forces the T-053
    /// software processing chain off to avoid processing the signal twice.
    AppleVoiceProcessing {
        /// Bypass Apple's processing while retaining the duplex Audio Unit.
        bypass: bool,
        /// Enable Apple's automatic gain control.
        automatic_gain_control: bool,
    },
}

/// How capture opens and closes a talk burst.
#[derive(Debug, Clone, Copy, Default)]
pub enum TalkGate {
    /// The host calls [`VoiceSession::set_ptt`].
    #[default]
    PushToTalk,
    /// T-052 energy/neural detection with pre-roll and hangover.
    VoiceActivation(vad::Config),
    /// Always transmit unless muted or deafened.
    OpenMic,
}

/// Device, processing, and gate settings for one call.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    /// Real devices, a deterministic tone, or silence.
    pub source: AudioSource,
    /// Input device; None follows the system default.
    pub input_device: Option<DeviceId>,
    /// Output device; None follows the system default.
    pub output_device: Option<DeviceId>,
    /// Portable devices or Apple's full-duplex voice-processing path.
    pub device_backend: DeviceBackend,
    /// Push-to-talk, VAD, or open microphone.
    pub gate: TalkGate,
    /// Tail retained after push-to-talk is released.
    pub release_delay: Duration,
    /// Optional AEC/noise-suppression/gain chain. Default is bypassed.
    pub processing: ProcessingConfig,
    /// Join self-muted.
    pub start_muted: bool,
    /// Join self-deafened.
    pub start_deafened: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            source: AudioSource::Device,
            input_device: None,
            output_device: None,
            device_backend: DeviceBackend::Portable,
            gate: TalkGate::PushToTalk,
            release_delay: ss_input::DEFAULT_RELEASE_DELAY,
            processing: ProcessingConfig::default(),
            start_muted: false,
            start_deafened: false,
        }
    }
}

/// UI sound injected before master gain and limiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Joined voice.
    Join,
    /// Left voice.
    Leave,
    /// Self-muted.
    Mute,
    /// Self-unmuted.
    Unmute,
}

/// One remote speaker's current playout diagnostics.
#[derive(Debug, Clone)]
pub struct SpeakerStats {
    /// Server-assigned track id.
    pub track: u32,
    /// Publishing user.
    pub user: Id,
    /// Applied jitter-buffer delay.
    pub jitter_delay: Duration,
    /// Source frames committed as lost.
    pub lost: u64,
    /// Decoder packet-loss-concealment calls.
    pub plc: u64,
    /// Arrival/flag based speaking indicator.
    pub speaking: bool,
}

/// Snapshot published every 500 ms through [`ClientEvent::VoiceStats`].
#[derive(Debug, Clone, Default)]
pub struct VoiceStats {
    /// Per-track receiver diagnostics.
    pub speakers: Vec<SpeakerStats>,
    /// Current encoder bitrate after adaptation.
    pub encoder_bitrate: u32,
    /// Smoothed QUIC round-trip time.
    pub rtt: Duration,
    /// Playback ring underruns.
    pub underruns: u64,
    /// Capture ring overruns.
    pub overruns: u64,
    /// Added processing latency.
    pub processing_latency: Duration,
    /// Processing frames that exceeded the real-time budget.
    pub processing_over_budget: u64,
    /// Latest mixed-output RMS; useful to headless clients and tests.
    pub output_rms: f32,
}

/// Device conditions that need UI action or explanation.
#[derive(Debug, Clone)]
pub enum VoiceDeviceState {
    /// Stored microphone permission denied capture.
    PermissionDenied,
    /// Capture delivered only zeroes for its first second.
    SilentInput {
        /// Whether the OS explicitly reports permission denied.
        permission_denied: bool,
    },
    /// A Bluetooth/hands-free path opened at speech-band quality.
    LowQualityInput {
        /// Granted hardware sample rate.
        sample_rate: u32,
    },
    /// Apple's voice-processing path is active; the software chain is off.
    AppleVoiceProcessing {
        /// Control Center mode currently effective on this audio route.
        microphone_mode: ss_audio::device::MicrophoneMode,
    },
    /// Apple detected speech while the processed microphone uplink was muted.
    MutedSpeech {
        /// `true` when speech began and `false` when it ended.
        active: bool,
    },
    /// A stream could not be reopened.
    Unavailable(String),
}

/// Owns one joined channel's device, codec, network, and stats workers.
pub struct VoiceSession {
    channel: Id,
    commands: tokio::sync::mpsc::Sender<Command>,
    store: StateStore,
    storage: Arc<dyn Storage>,
    server: ss_net::pins::ServerId,
    worker: Option<WorkerHandle>,
    tasks: Vec<JoinHandle<()>>,
    stats: Arc<RwLock<VoiceStats>>,
}

impl VoiceSession {
    /// Join a channel, start devices/codecs, and bind them to this client.
    pub async fn start(client: &Client, channel: Id, config: AudioConfig) -> Result<Self, Error> {
        let (profile, bitrate) = channel_profile(&client.store, channel)?;
        let mut events = client.subscribe();
        client
            .join_voice(v1::JoinVoice {
                channel_id: id_to_bytes(channel),
                self_mute: config.start_muted,
                self_deaf: config.start_deafened,
            })
            .await?;
        let deadline = tokio::time::Instant::now() + super::connection::DEADLINE;
        let initial = loop {
            let tracks = load_tracks(
                &client.store,
                client.storage.clone(),
                client.address.id()?,
                channel,
            )
            .await?;
            if tracks.own != 0 {
                break tracks;
            }
            tokio::time::timeout_at(deadline, events.recv())
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(|_| Error::Disconnected)?;
        };
        let worker = match worker::start(
            profile,
            bitrate,
            config,
            client.media.clone(),
            client.events.clone(),
        )
        .await
        {
            Ok(worker) => worker,
            Err(error) => {
                let _ = client.leave_voice(v1::LeaveVoice {}).await;
                return Err(Error::Voice(error));
            }
        };
        let _ = worker.updates.try_send(Update::Tracks {
            own: initial.own,
            remote: initial.remote,
        });
        let stats = Arc::new(RwLock::new(VoiceStats::default()));
        let tasks = spawn_tasks(
            client,
            channel,
            &worker,
            Arc::clone(&stats),
            events,
            profile,
            bitrate,
        );
        Ok(Self {
            channel,
            commands: client.commands.clone(),
            store: client.store.clone(),
            storage: client.storage.clone(),
            server: client.address.id()?,
            worker: Some(worker),
            tasks,
            stats,
        })
    }

    /// Current joined channel.
    pub const fn channel(&self) -> Id {
        self.channel
    }

    /// Open or release the external push-to-talk gate.
    pub fn set_ptt(&self, pressed: bool) {
        if let Some(worker) = &self.worker {
            worker.set_ptt(pressed);
        }
    }

    /// Switch between portable devices and Apple's VoiceProcessingIO backend
    /// without leaving the voice channel.
    pub async fn set_device_backend(&self, backend: DeviceBackend) -> Result<(), Error> {
        let worker = self
            .worker
            .as_ref()
            .ok_or_else(|| Error::Voice("voice worker is stopped".to_owned()))?;
        let (reply, response) = oneshot::channel();
        worker
            .updates
            .try_send(Update::DeviceBackend { backend, reply })
            .map_err(|_| Error::Voice("voice worker update queue is unavailable".to_owned()))?;
        response
            .await
            .map_err(|_| Error::Voice("voice worker stopped while switching devices".to_owned()))?
            .map_err(Error::Voice)
    }

    /// Update self mute locally and on the server.
    pub async fn set_self_mute(&self, muted: bool) -> Result<(), Error> {
        let deaf = self.worker.as_ref().is_some_and(|worker| {
            worker
                .shared
                .deaf
                .load(std::sync::atomic::Ordering::Relaxed)
        });
        self.set_voice_state(muted, deaf).await?;
        if let Some(worker) = &self.worker {
            worker
                .shared
                .muted
                .store(muted, std::sync::atomic::Ordering::Relaxed);
            let _ =
                worker
                    .updates
                    .try_send(Update::Cue(if muted { Cue::Mute } else { Cue::Unmute }));
        }
        Ok(())
    }

    /// Update self deafen locally and on the server. Deafening also gates send.
    pub async fn set_self_deaf(&self, deaf: bool) -> Result<(), Error> {
        let muted = self.worker.as_ref().is_some_and(|worker| {
            worker
                .shared
                .muted
                .load(std::sync::atomic::Ordering::Relaxed)
        });
        self.set_voice_state(muted, deaf).await?;
        if let Some(worker) = &self.worker {
            worker
                .shared
                .deaf
                .store(deaf, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    /// Persist and apply a remote user's 0–200% volume.
    pub async fn set_user_volume(&self, user: Id, percent: u16) -> Result<(), Error> {
        if percent > 200 {
            return Err(Error::Voice(
                "user volume must be within 0..=200".to_owned(),
            ));
        }
        let gain = f32::from(percent) / 100.0;
        let storage = self.storage.clone();
        let server = self.server.clone();
        let muted = tokio::task::spawn_blocking(move || {
            storage.save_user_volume(&server, user, gain)?;
            storage.user_muted(&server, user)
        })
        .await
        .map_err(|error| Error::Storage(error.to_string()))??;
        if let Some(worker) = &self.worker {
            let _ = worker.updates.try_send(Update::User { user, gain, muted });
        }
        Ok(())
    }

    /// Persist and apply a local-only mute for a remote user.
    pub async fn set_user_muted(&self, user: Id, muted: bool) -> Result<(), Error> {
        let storage = self.storage.clone();
        let server = self.server.clone();
        let gain = tokio::task::spawn_blocking(move || {
            storage.save_user_muted(&server, user, muted)?;
            Ok::<_, Error>(storage.user_volume(&server, user)?.unwrap_or(1.0))
        })
        .await
        .map_err(|error| Error::Storage(error.to_string()))??;
        if let Some(worker) = &self.worker {
            let _ = worker.updates.try_send(Update::User { user, gain, muted });
        }
        Ok(())
    }

    /// Inject a UI cue into the mixer.
    pub fn play_cue(&self, cue: Cue) {
        if let Some(worker) = &self.worker {
            let _ = worker.updates.try_send(Update::Cue(cue));
        }
    }

    /// Latest 500 ms diagnostics snapshot.
    pub fn stats(&self) -> VoiceStats {
        self.stats.read().clone()
    }

    /// Leave voice and wait for every worker to release its stream/thread.
    pub async fn leave(mut self) -> Result<(), Error> {
        let result = self
            .command(v1::client_envelope::Msg::LeaveVoice(v1::LeaveVoice {}))
            .await;
        self.shutdown().await;
        result.map(|_| ())
    }

    async fn set_voice_state(&self, muted: bool, deaf: bool) -> Result<(), Error> {
        self.command(v1::client_envelope::Msg::SetVoiceState(v1::SetVoiceState {
            self_mute: muted,
            self_deaf: deaf,
        }))
        .await
        .map(|_| ())
    }

    async fn command(&self, message: v1::client_envelope::Msg) -> Result<v1::Ack, Error> {
        let generation = {
            let state = self.store.read();
            if state.connection != ConnectionState::Connected {
                return Err(Error::Disconnected);
            }
            state.generation
        };
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command {
                message,
                generation,
                replayable: false,
                reply,
            })
            .map_err(|_| Error::Disconnected)?;
        response.await.map_err(|_| Error::Disconnected)?
    }

    async fn shutdown(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        if let Some(worker) = self.worker.take() {
            worker.shutdown().await;
        }
    }
}

impl Drop for VoiceSession {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        self.worker.take();
    }
}

fn channel_profile(store: &StateStore, channel: Id) -> Result<(VoiceProfile, u32), Error> {
    let state = store.read();
    let channel = state
        .channels
        .get(&channel)
        .ok_or(Error::Protocol("unknown voice channel"))?;
    if channel.kind != v1::ChannelKind::Voice as i32 {
        return Err(Error::Protocol("channel is not voice"));
    }
    let settings = channel.voice.as_ref();
    let profile = match settings
        .and_then(|settings| v1::VoiceProfile::try_from(settings.profile).ok())
        .unwrap_or(v1::VoiceProfile::Voice)
    {
        v1::VoiceProfile::Music => VoiceProfile::Music,
        v1::VoiceProfile::LowLatency => VoiceProfile::LowLatency,
        v1::VoiceProfile::Unspecified | v1::VoiceProfile::Voice => VoiceProfile::Voice,
    };
    let params = profile.encoder_params();
    let bitrate = settings
        .and_then(|settings| settings.bitrate)
        .unwrap_or(params.default_bitrate_bps);
    if !params.contains_bitrate(bitrate) {
        return Err(Error::Voice(
            "channel bitrate is outside its profile range".to_owned(),
        ));
    }
    Ok((profile, bitrate))
}

struct Tracks {
    own: u32,
    remote: Vec<TrackSpec>,
}

async fn load_tracks(
    store: &StateStore,
    storage: Arc<dyn Storage>,
    server: ss_net::pins::ServerId,
    channel: Id,
) -> Result<Tracks, Error> {
    let (own_user, tracks) = {
        let state = store.read();
        let own_user = state
            .own_user
            .as_ref()
            .map(|user| id(&user.id))
            .transpose()?
            .ok_or(Error::Protocol("missing own user"))?;
        let mut tracks = Vec::new();
        for track in state.tracks.values() {
            if track.kind != v1::TrackKind::Audio as i32 || id(&track.channel_id)? != channel {
                continue;
            }
            tracks.push((track.track_id, id(&track.user_id)?));
        }
        (own_user, tracks)
    };
    let own = tracks
        .iter()
        .filter(|(_, user)| *user == own_user)
        .map(|(track, _)| *track)
        .max()
        .unwrap_or(0);
    let users: BTreeSet<_> = tracks
        .iter()
        .filter(|(track, _)| *track != own)
        .map(|(_, user)| *user)
        .collect();
    let settings = tokio::task::spawn_blocking(move || {
        let mut settings = BTreeMap::new();
        for user in users {
            settings.insert(
                user,
                (
                    storage
                        .user_volume(&server, user)?
                        .unwrap_or(1.0)
                        .clamp(0.0, 2.0),
                    storage.user_muted(&server, user)?,
                ),
            );
        }
        Ok::<_, Error>(settings)
    })
    .await
    .map_err(|error| Error::Storage(error.to_string()))??;
    let remote = tracks
        .into_iter()
        .filter(|(track, _)| *track != own)
        .map(|(track, user)| {
            let (gain, muted) = settings.get(&user).copied().unwrap_or((1.0, false));
            TrackSpec {
                id: track,
                user,
                gain,
                muted,
            }
        })
        .collect();
    Ok(Tracks { own, remote })
}

#[allow(clippy::too_many_arguments)]
fn spawn_tasks(
    client: &Client,
    channel: Id,
    worker: &WorkerHandle,
    published: Arc<RwLock<VoiceStats>>,
    mut events: broadcast::Receiver<ClientEvent>,
    profile: VoiceProfile,
    bitrate: u32,
) -> Vec<JoinHandle<()>> {
    let mut incoming = client.media.subscribe();
    let incoming_tx = worker.incoming.clone();
    let media_task = tokio::spawn(async move {
        loop {
            match incoming.recv().await {
                Ok(packet) => {
                    let _ = incoming_tx.try_send(packet);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let store = client.store.clone();
    let storage = client.storage.clone();
    let server = client.address.id().ok();
    let updates = worker.updates.clone();
    let track_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            if !matches!(event, ClientEvent::VoiceChanged | ClientEvent::Resynced) {
                continue;
            }
            let Some(server) = server.clone() else { break };
            if let Ok(tracks) = load_tracks(&store, storage.clone(), server, channel).await {
                let _ = updates.try_send(Update::Tracks {
                    own: tracks.own,
                    remote: tracks.remote,
                });
            }
        }
    });

    let shared = Arc::clone(&worker.shared);
    let updates = worker.updates.clone();
    let media = client.media.clone();
    let event_tx = client.events.clone();
    let stats_task = tokio::spawn(async move {
        let params = profile.encoder_params();
        let mut current = bitrate;
        let mut previous_lost = 0;
        let mut previous_sent = 0;
        let mut bad_ticks = 0_u8;
        let mut clean_ticks = 0_u8;
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let worker_stats = shared.stats.read().clone();
            let transport = media.stats();
            let delta_lost = transport.lost_packets.saturating_sub(previous_lost);
            let delta_sent = worker_stats
                .graph
                .sent_packets
                .saturating_sub(previous_sent);
            previous_lost = transport.lost_packets;
            previous_sent = worker_stats.graph.sent_packets;
            let high_loss =
                delta_sent > 0 && delta_lost.saturating_mul(100) > delta_sent.saturating_mul(8);
            if high_loss {
                bad_ticks = bad_ticks.saturating_add(1);
                clean_ticks = 0;
            } else if delta_lost == 0 {
                clean_ticks = clean_ticks.saturating_add(1);
                bad_ticks = 0;
            } else {
                bad_ticks = 0;
                clean_ticks = 0;
            }
            if bad_ticks >= 10 && current > params.min_bitrate_bps {
                current = (current.saturating_mul(4) / 5).max(params.min_bitrate_bps);
                let _ = updates.try_send(Update::Bitrate(current));
                bad_ticks = 0;
            } else if clean_ticks >= 60 && current < bitrate {
                current = bitrate;
                let _ = updates.try_send(Update::Bitrate(current));
                clean_ticks = 0;
            }
            let snapshot = public_stats(&worker_stats, transport);
            *published.write() = snapshot.clone();
            let _ = event_tx.send(ClientEvent::VoiceStats(snapshot));
        }
    });
    vec![media_task, track_task, stats_task]
}

fn public_stats(stats: &worker::WorkerStats, transport: TransportStats) -> VoiceStats {
    let mut speakers: Vec<_> = stats
        .graph
        .tracks
        .iter()
        .filter_map(|(&track, snapshot)| {
            Some(SpeakerStats {
                track,
                user: snapshot.user?,
                jitter_delay: snapshot.delay,
                lost: snapshot.stats.jitter.lost,
                plc: snapshot.stats.plc,
                speaking: snapshot.speaking,
            })
        })
        .collect();
    speakers.sort_by_key(|speaker| speaker.track);
    VoiceStats {
        speakers,
        encoder_bitrate: stats.graph.bitrate,
        rtt: transport.rtt,
        underruns: stats.underruns,
        overruns: stats.overruns,
        processing_latency: stats.processing_latency,
        processing_over_budget: stats.processing_over_budget,
        output_rms: stats.graph.output_rms,
    }
}
