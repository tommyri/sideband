//! Codec, jitter, and mixing graph owned by the voice worker.

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use ss_audio::{
    codec::{self, Encoder},
    jitter::PacketHeader,
    mixer::{Mixer, StereoFrame},
    receive::{Stats as ReceiverStats, TrackReceiver},
};
use ss_core::{Id, VoiceProfile};
use ss_net::media::{self, DatagramHeader, TALK_END, TALK_START};

use super::Cue;

const UNKNOWN_TTL: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub(super) enum GraphError {
    #[error("capture quantum has the wrong length")]
    Frame,
    #[error(transparent)]
    Codec(#[from] codec::Error),
    #[error(transparent)]
    Media(#[from] media::MediaError),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct TrackSpec {
    pub id: u32,
    pub user: Id,
    pub gain: f32,
    pub muted: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct TrackSnapshot {
    pub user: Option<Id>,
    pub delay: Duration,
    pub stats: ReceiverStats,
    pub speaking: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct GraphSnapshot {
    pub talking: bool,
    pub sent_bytes: u64,
    pub sent_packets: u64,
    pub bitrate: u32,
    pub output_rms: f32,
    pub tracks: HashMap<u32, TrackSnapshot>,
}

struct Track {
    user: Id,
    receiver: TrackReceiver,
    gain: f32,
    muted: bool,
}

struct Pending {
    bytes: Bytes,
    arrival: Duration,
}

pub(super) struct VoiceGraph {
    channels: usize,
    quantum: usize,
    frame_samples: usize,
    samples_per_channel: u32,
    encoder: Encoder,
    pending: Vec<f32>,
    frame: Vec<f32>,
    seq: u16,
    timestamp: u32,
    track_id: u32,
    talking: bool,
    outgoing: Vec<Bytes>,
    tracks: HashMap<u32, Track>,
    unknown: HashMap<u32, VecDeque<Pending>>,
    mixer: Mixer,
    cues: VecDeque<StereoFrame>,
    preferences: super::AudioConfig,
    profile: VoiceProfile,
    snapshot: GraphSnapshot,
}

impl VoiceGraph {
    pub fn new(profile: VoiceProfile, bitrate: u32) -> Result<Self, GraphError> {
        let params = profile.encoder_params();
        let channels = usize::from(params.channels);
        let millis = u32::try_from(params.frame_size.as_millis()).unwrap_or(20);
        let samples_per_channel = millis.saturating_mul(48);
        let frame_samples = usize::try_from(samples_per_channel).unwrap_or(960) * channels;
        let mut encoder = Encoder::new(profile)?;
        encoder.set_bitrate(bitrate)?;
        Ok(Self {
            channels,
            quantum: 480 * channels,
            frame_samples,
            samples_per_channel,
            encoder,
            pending: Vec::with_capacity(frame_samples + 480 * channels),
            frame: vec![0.0; frame_samples],
            seq: 0,
            timestamp: 0,
            track_id: 0,
            talking: false,
            outgoing: Vec::new(),
            tracks: HashMap::new(),
            unknown: HashMap::new(),
            mixer: Mixer::new(),
            cues: VecDeque::new(),
            preferences: super::AudioConfig::default(),
            profile,
            snapshot: GraphSnapshot {
                bitrate,
                ..GraphSnapshot::default()
            },
        })
    }

    pub const fn channels(&self) -> usize {
        self.channels
    }

    pub const fn quantum(&self) -> usize {
        self.quantum
    }

    pub fn set_track_id(&mut self, track_id: u32) {
        self.track_id = track_id;
        self.talking = false;
    }

    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<(), GraphError> {
        self.encoder.set_bitrate(bitrate)?;
        self.snapshot.bitrate = bitrate;
        Ok(())
    }

    pub fn sync_tracks(&mut self, specs: &[TrackSpec], now: Duration) {
        self.tracks
            .retain(|id, _| specs.iter().any(|spec| spec.id == *id));
        for spec in specs {
            if spec.id == 0 || spec.id == self.track_id {
                continue;
            }
            if let Some(track) = self.tracks.get_mut(&spec.id) {
                track.gain = spec.gain;
                track.muted = spec.muted;
                continue;
            }
            let Ok(mut receiver) = TrackReceiver::new(self.profile) else {
                continue;
            };
            receiver.set_jitter_bounds(self.preferences.jitter_min, self.preferences.jitter_max);
            self.tracks.insert(
                spec.id,
                Track {
                    user: spec.user,
                    receiver,
                    gain: spec.gain,
                    muted: spec.muted,
                },
            );
            if let Some(mut packets) = self.unknown.remove(&spec.id) {
                while let Some(packet) = packets.pop_front() {
                    if now.saturating_sub(packet.arrival) <= UNKNOWN_TTL {
                        self.push_datagram(&packet.bytes, packet.arrival);
                    }
                }
            }
        }
    }

    pub fn configure(&mut self, config: &super::AudioConfig) {
        self.preferences = config.clone();
        let _ = self.mixer.set_master_gain(config.master_gain);
        for track in self.tracks.values_mut() {
            track
                .receiver
                .set_jitter_bounds(config.jitter_min, config.jitter_max);
        }
    }

    pub fn set_user(&mut self, user: Id, gain: f32, muted: bool) {
        for track in self.tracks.values_mut().filter(|track| track.user == user) {
            track.gain = gain;
            track.muted = muted;
        }
    }

    pub fn push_capture(&mut self, input: &[f32], gate: bool) -> Result<(), GraphError> {
        if input.len() != self.quantum {
            return Err(GraphError::Frame);
        }
        self.pending.extend_from_slice(input);
        while self.pending.len() >= self.frame_samples {
            self.finish_frame(gate)?;
        }
        Ok(())
    }

    pub fn pop_outgoing(&mut self) -> Vec<Bytes> {
        std::mem::take(&mut self.outgoing)
    }

    pub fn push_datagram(&mut self, bytes: &[u8], arrival: Duration) {
        let Ok((header, payload)) = DatagramHeader::parse(bytes) else {
            return;
        };
        if header.track_id == self.track_id {
            return;
        }
        let Some(track) = self.tracks.get_mut(&header.track_id) else {
            let queue = self.unknown.entry(header.track_id).or_default();
            if queue.len() < 32 {
                queue.push_back(Pending {
                    bytes: Bytes::copy_from_slice(bytes),
                    arrival,
                });
            }
            return;
        };
        let header = PacketHeader {
            seq: header.seq,
            timestamp: header.timestamp,
            talk_start: header.flags & TALK_START != 0,
            talk_end: header.flags & TALK_END != 0,
        };
        let _ = track.receiver.push(header, payload, arrival);
    }

    pub fn play_cue(&mut self, cue: Cue) {
        let frequency = match cue {
            Cue::Join => 660.0,
            Cue::Leave => 440.0,
            Cue::Mute => 330.0,
            Cue::Unmute => 550.0,
        };
        let mut frame = [0.0; 960];
        for (index, pair) in frame.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let phase = index as f32 * frequency * std::f32::consts::TAU / 48_000.0;
            *pair = [phase.sin() * 0.08; 2];
        }
        self.cues.push_back(frame);
    }

    pub fn render(&mut self, now: Duration, deaf: bool) -> StereoFrame {
        let mut snapshots = HashMap::with_capacity(self.tracks.len());
        for (&id, track) in &mut self.tracks {
            let frame = *track.receiver.render(now);
            let speaking = track.receiver.speaking(now);
            if !deaf && !track.muted {
                let _ = self.mixer.add_track(
                    &frame,
                    track.gain
                        * if self.talking {
                            1.0 - self.preferences.attenuation
                        } else {
                            1.0
                        },
                );
            }
            snapshots.insert(
                id,
                TrackSnapshot {
                    user: Some(track.user),
                    delay: track.receiver.applied_delay(),
                    stats: track.receiver.stats(),
                    speaking,
                },
            );
        }
        if !deaf && let Some(cue) = self.cues.pop_front() {
            let _ = self.mixer.add_track(&cue, self.preferences.cue_gain);
        }
        let output = if deaf {
            let _ = self.mixer.finish();
            [0.0; 960]
        } else {
            *self.mixer.finish()
        };
        self.unknown.retain(|_, packets| {
            packets.retain(|packet| now.saturating_sub(packet.arrival) <= UNKNOWN_TTL);
            !packets.is_empty()
        });
        let mean = output
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>()
            / 960.0;
        #[allow(clippy::cast_possible_truncation)]
        let output_rms = mean.sqrt() as f32;
        self.snapshot.output_rms = output_rms;
        self.snapshot.tracks = snapshots;
        output
    }

    pub fn snapshot(&self) -> GraphSnapshot {
        let mut snapshot = self.snapshot.clone();
        snapshot.talking = self.talking;
        snapshot
    }

    fn finish_frame(&mut self, gate: bool) -> Result<(), GraphError> {
        self.frame
            .copy_from_slice(&self.pending[..self.frame_samples]);
        self.pending.drain(..self.frame_samples);
        let timestamp = self.timestamp;
        self.timestamp = self.timestamp.wrapping_add(self.samples_per_channel);
        if !gate && !self.talking {
            return Ok(());
        }
        let mut flags = 0;
        if !self.talking {
            flags |= TALK_START;
        }
        if !gate {
            flags |= TALK_END;
        }
        self.talking = gate;
        if self.track_id == 0 {
            return Ok(());
        }
        let payload = self.encoder.encode(&self.frame)?.to_vec();
        let packet = audio_packet(self.track_id, self.seq, timestamp, flags, &payload)?;
        self.seq = self.seq.wrapping_add(1);
        self.snapshot.sent_bytes = self
            .snapshot
            .sent_bytes
            .saturating_add(u64::try_from(payload.len()).unwrap_or(0));
        self.snapshot.sent_packets = self.snapshot.sent_packets.saturating_add(1);
        self.outgoing.push(packet);
        Ok(())
    }
}

fn audio_packet(
    track_id: u32,
    seq: u16,
    timestamp: u32,
    flags: u8,
    payload: &[u8],
) -> Result<Bytes, media::MediaError> {
    let mut bytes = BytesMut::with_capacity(16 + payload.len());
    DatagramHeader {
        version: 0,
        kind: 0,
        flags,
        track_id,
        seq,
        timestamp,
    }
    .encode_into(&mut bytes)?;
    bytes.extend_from_slice(payload);
    Ok(bytes.freeze())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_and_cue_volume_apply_before_playback() {
        let mut graph = VoiceGraph::new(VoiceProfile::Voice, 80_000).unwrap();
        graph.configure(&super::super::AudioConfig {
            cue_gain: 0.0,
            ..Default::default()
        });
        graph.play_cue(Cue::Join);
        assert!(
            graph
                .render(Duration::ZERO, false)
                .iter()
                .all(|s| *s == 0.0)
        );
        graph.configure(&super::super::AudioConfig::default());
        graph.play_cue(Cue::Join);
        assert!(
            graph
                .render(Duration::ZERO, false)
                .iter()
                .any(|s| s.abs() > 0.0)
        );
        graph.configure(&super::super::AudioConfig {
            master_gain: 0.0,
            ..Default::default()
        });
        graph.play_cue(Cue::Join);
        assert!(
            graph
                .render(Duration::ZERO, false)
                .iter()
                .all(|s| *s == 0.0)
        );
    }

    fn tone(frame: &mut [f32], channels: usize, start: usize) {
        for index in 0..frame.len() / channels {
            #[allow(clippy::cast_precision_loss)]
            let phase = (start + index) as f32 * 440.0 * std::f32::consts::TAU / 48_000.0;
            for channel in 0..channels {
                frame[index * channels + channel] = phase.sin() * 0.25;
            }
        }
    }

    #[test]
    fn tone_crosses_the_graph_and_deafen_silences_it() {
        let profile = VoiceProfile::Voice;
        let mut sender = VoiceGraph::new(profile, 80_000).unwrap();
        let mut receiver = VoiceGraph::new(profile, 80_000).unwrap();
        sender.set_track_id(1);
        receiver.set_track_id(2);
        receiver.sync_tracks(
            &[TrackSpec {
                id: 1,
                user: Id::generate(),
                gain: 1.0,
                muted: false,
            }],
            Duration::ZERO,
        );
        let mut frame = vec![0.0; sender.quantum()];
        let mut arrival = Duration::from_millis(20);
        for index in 0..24 {
            tone(&mut frame, sender.channels(), index * 480);
            sender.push_capture(&frame, true).unwrap();
            for packet in sender.pop_outgoing() {
                receiver.push_datagram(&packet, arrival);
                arrival += Duration::from_millis(20);
            }
        }
        let mut peak = 0.0_f32;
        for step in 0..60 {
            receiver.render(Duration::from_millis(step * 10), false);
            peak = peak.max(receiver.snapshot().output_rms);
        }
        assert!(peak > 0.01);
        assert_eq!(
            receiver.render(Duration::from_millis(610), true),
            [0.0; 960]
        );
    }
}
