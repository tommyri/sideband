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
/// Samples per channel in one 10 ms capture quantum.
const QUANTUM_SAMPLES: u32 = 480;

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
    pub timestamp: Option<u32>,
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
    voiced: bool,
    retractable: usize,
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
            voiced: false,
            retractable: 0,
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

    /// Push one 10 ms capture quantum. Call once per captured quantum whatever
    /// the gate says, so the capture clock keeps running while it is closed.
    ///
    /// A closed quantum is never transmitted: inside a burst it becomes
    /// silence that completes the `TALK_END` frame (which also flushes the
    /// encoder), and between bursts it only advances the timestamp, so the
    /// next burst starts on any quantum with a timestamp gap.
    pub fn push_capture(&mut self, input: &[f32], gate: bool) -> Result<(), GraphError> {
        if input.len() != self.quantum {
            return Err(GraphError::Frame);
        }
        if gate {
            self.pending.extend_from_slice(input);
            self.voiced = true;
            self.retractable = 0;
        } else if self.talking || !self.pending.is_empty() {
            self.pending.resize(self.pending.len() + self.quantum, 0.0);
            self.retractable += 1;
        } else {
            self.timestamp = self.timestamp.wrapping_add(QUANTUM_SAMPLES);
            self.retractable += 1;
        }
        while self.pending.len() >= self.frame_samples {
            self.finish_frame(gate)?;
        }
        Ok(())
    }

    /// Take back up to `quanta` of the most recent closed quanta that have not
    /// been sent. Voice activation replays them as pre-roll when it opens;
    /// without this the capture clock would count them twice per burst.
    pub fn retract_closed(&mut self, quanta: usize) {
        let count = quanta.min(self.retractable);
        self.retractable -= count;
        for _ in 0..count {
            // Unsent closed quanta are either the silent tail of `pending` or,
            // between bursts, timestamp advances with `pending` empty.
            if self.pending.len() >= self.quantum {
                self.pending.truncate(self.pending.len() - self.quantum);
            } else {
                self.timestamp = self.timestamp.wrapping_sub(QUANTUM_SAMPLES);
            }
        }
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
        // A burst must not create a seconds-long backlog of stale sounds.
        if self.cues.len() <= 12 {
            self.cues.extend(cue.frames());
        }
    }
    pub fn clear_cues(&mut self) {
        self.cues.clear();
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
                    timestamp: track
                        .receiver
                        .playout_timestamp()
                        .map(|t| t.wrapping_sub(480)),
                    delay: track.receiver.applied_delay(),
                    stats: track.receiver.stats(),
                    speaking,
                },
            );
        }
        if let Some(cue) = self.cues.pop_front() {
            let _ = self.mixer.add_track(&cue, self.preferences.cue_gain);
        }
        let output = *self.mixer.finish();
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

    pub fn capture_timestamp(&self) -> u32 {
        self.timestamp
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
        self.retractable = 0;
        let voiced = std::mem::take(&mut self.voiced);
        if !gate && !self.talking && !voiced {
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
pub(super) mod tests {
    use super::*;

    /// A burst's expected first-sample timestamp and the capture position just
    /// after its last gated quantum, both in 48 kHz samples.
    #[derive(Debug, Clone, Copy)]
    pub(in crate::voice) struct Expected {
        pub start: u32,
        pub gated_end: u32,
    }

    /// Every burst starts with `TALK_START` at the capture position of its
    /// first sample, is contiguous, and ends with exactly one `TALK_END` in
    /// the frame that holds the first closed quantum.
    pub(in crate::voice) fn assert_bursts(packets: &[Bytes], frame: u32, expected: &[Expected]) {
        let headers: Vec<_> = packets
            .iter()
            .map(|packet| DatagramHeader::parse(packet).unwrap().0)
            .collect();
        let mut bursts: Vec<Vec<DatagramHeader>> = Vec::new();
        for header in &headers {
            if header.flags & TALK_START != 0 {
                bursts.push(Vec::new());
            }
            bursts
                .last_mut()
                .expect("the first packet must carry TALK_START")
                .push(*header);
        }
        assert_eq!(bursts.len(), expected.len(), "bursts: {headers:?}");
        let mut seq = headers[0].seq;
        let mut previous_end: Option<u32> = None;
        for (burst, expected) in bursts.iter().zip(expected) {
            assert_eq!(burst[0].timestamp, expected.start, "burst start: {burst:?}");
            if let Some(end) = previous_end {
                assert!(
                    expected.start >= end,
                    "burst overlaps one ending at {end}: {burst:?}"
                );
            }
            let mut timestamp = expected.start;
            for (index, header) in burst.iter().enumerate() {
                assert_eq!(header.seq, seq);
                assert_eq!(header.timestamp, timestamp);
                assert_eq!(
                    header.flags & TALK_END != 0,
                    index + 1 == burst.len(),
                    "exactly one TALK_END, on the last packet: {burst:?}"
                );
                seq = seq.wrapping_add(1);
                timestamp = timestamp.wrapping_add(frame);
            }
            assert!(
                timestamp > expected.gated_end && timestamp <= expected.gated_end + frame,
                "TALK_END frame ends at {timestamp}, gated audio at {}",
                expected.gated_end
            );
            previous_end = Some(timestamp);
        }
    }

    /// Bursts of 1..=5 quanta (odd and even) separated by `gap` closed quanta.
    /// A 20 ms frame needs two closed quanta to end a burst.
    fn gate_script(gap: usize) -> (Vec<bool>, Vec<Expected>) {
        let mut gates = vec![false; gap];
        let mut expected = Vec::new();
        for length in 1..=5 {
            let start = u32::try_from(gates.len() * 480).unwrap();
            gates.extend(std::iter::repeat_n(true, length));
            expected.push(Expected {
                start,
                gated_end: u32::try_from(gates.len() * 480).unwrap(),
            });
            gates.extend(std::iter::repeat_n(false, gap));
        }
        (gates, expected)
    }

    #[test]
    fn every_burst_ends_with_talk_end_and_the_next_starts_after_a_gap() {
        for profile in [
            VoiceProfile::Voice,
            VoiceProfile::Music,
            VoiceProfile::LowLatency,
        ] {
            for gap in [2, 3, 7] {
                let mut graph =
                    VoiceGraph::new(profile, profile.encoder_params().default_bitrate_bps).unwrap();
                graph.set_track_id(1);
                let (gates, expected) = gate_script(gap);
                let speech = vec![0.1; graph.quantum()];
                let mut packets = Vec::new();
                for gate in gates {
                    graph.push_capture(&speech, gate).unwrap();
                    packets.extend(graph.pop_outgoing());
                }
                assert!(!graph.snapshot().talking, "{profile:?} gap {gap}");
                assert_bursts(&packets, graph.samples_per_channel, &expected);
            }
        }
    }

    #[test]
    fn closing_quantum_is_zeroed_even_when_the_microphone_still_hears_speech() {
        for profile in [VoiceProfile::Voice, VoiceProfile::LowLatency] {
            for open in [1, 2, 3] {
                let mut graph = VoiceGraph::new(profile, 64_000).unwrap();
                graph.set_track_id(1);
                let speech = vec![0.1; graph.quantum()];
                for _ in 0..open {
                    graph.push_capture(&speech, true).unwrap();
                }
                let mut closed = 0;
                while graph.snapshot().talking {
                    graph.push_capture(&speech, false).unwrap();
                    closed += 1;
                }
                let tail = (closed * graph.quantum()).min(graph.frame_samples);
                let frame = &graph.frame[graph.frame_samples - tail..];
                assert!(frame.iter().all(|s| *s == 0.0), "{profile:?} open {open}");
            }
        }
    }

    #[test]
    fn cue_queue_is_bounded_and_dnd_clear_discards_pending_samples() {
        let mut graph = VoiceGraph::new(VoiceProfile::Voice, 80_000).unwrap();
        for _ in 0..100 {
            graph.play_cue(Cue::Mention);
        }
        assert!(graph.cues.len() <= 18);
        graph.clear_cues();
        assert!(
            graph
                .render(Duration::ZERO, false)
                .iter()
                .all(|s| *s == 0.0)
        );
        graph.play_cue(Cue::Deafen);
        assert!(graph.render(Duration::ZERO, true).iter().any(|s| *s != 0.0));
    }
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

    fn flags(graph: &mut VoiceGraph) -> Vec<(u8, u32)> {
        graph
            .pop_outgoing()
            .iter()
            .map(|packet| {
                let header = DatagramHeader::parse(packet).unwrap().0;
                (header.flags, header.timestamp)
            })
            .collect()
    }

    #[test]
    fn pre_roll_retraction_rewinds_only_unsent_closed_quanta() {
        let mut graph = VoiceGraph::new(VoiceProfile::Voice, 64_000).unwrap();
        graph.set_track_id(1);
        let speech = vec![0.1; graph.quantum()];
        for gate in [true, true, false, false, false] {
            graph.push_capture(&speech, gate).unwrap();
        }
        assert_eq!(flags(&mut graph), [(TALK_START, 0), (TALK_END, 960)]);
        // One closed quantum follows the TALK_END frame; only it is unsent.
        graph.retract_closed(2);
        assert_eq!(graph.capture_timestamp(), 1920);
        graph.push_capture(&speech, true).unwrap();
        graph.push_capture(&speech, true).unwrap();
        assert_eq!(flags(&mut graph), [(TALK_START, 1920)]);

        // Inside a burst, a closed quantum still waiting in `pending` is
        // replaced by the replayed audio and the burst simply continues.
        graph.push_capture(&speech, false).unwrap();
        graph.retract_closed(1);
        graph.push_capture(&speech, true).unwrap();
        graph.push_capture(&speech, true).unwrap();
        assert_eq!(flags(&mut graph), [(0, 2880)]);
        assert!(graph.snapshot().talking);
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
