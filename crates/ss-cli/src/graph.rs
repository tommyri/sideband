//! Send and receive path for one joined client. Capture quanta are 10 ms.
//! The encoder frame follows the voice profile. Playback is always one stereo
//! quantum from the mixer.

use std::{collections::HashMap, time::Duration};

use bytes::{Bytes, BytesMut};
use ss_audio::{
    codec::{self, Encoder},
    jitter::PacketHeader,
    mixer::{Mixer, StereoFrame},
    receive::TrackReceiver,
};
use ss_core::VoiceProfile;
use ss_net::media::{self, DatagramHeader, TALK_END, TALK_START};

use crate::measure::mix_click;

/// A capture quantum or encoded packet could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// The capture slice was not one 10 ms quantum.
    #[error("capture quantum has the wrong length")]
    Frame,
    /// Opus rejected the frame or the profile.
    #[error(transparent)]
    Codec(#[from] codec::Error),
    /// The datagram header could not be written.
    #[error(transparent)]
    Media(#[from] media::MediaError),
}

/// Counters the stats line prints. Loss and PLC accumulate for the call.
#[derive(Debug, Clone, Copy, Default)]
pub struct Snapshot {
    /// Opus payload bytes, excluding the datagram header.
    pub sent_bytes: u64,
    /// Jitter slots committed as lost, summed across tracks.
    pub lost: u64,
    /// Packets admitted to a jitter buffer.
    pub accepted: u64,
    /// Decoder PLC calls.
    pub plc: u64,
    /// Largest applied jitter delay.
    pub buffer: Duration,
    /// Tracks whose speaking flag is set.
    pub speakers: usize,
}

/// One client's codecs, jitter buffers, and mixer.
#[allow(clippy::struct_excessive_bools)]
pub struct VoiceGraph {
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
    gate: bool,
    talking: bool,
    click_due: bool,
    frame_has_click: bool,
    quanta: u64,
    measure: bool,
    outgoing: Vec<Bytes>,
    tracks: HashMap<u32, TrackReceiver>,
    mixer: Mixer,
    sent_bytes: u64,
    profile: VoiceProfile,
    snapshot: Snapshot,
}

impl VoiceGraph {
    /// Build the encoder for `profile`. `track_id` is the server assignment
    /// written into outgoing datagrams. Zero suppresses sending.
    pub fn new(profile: VoiceProfile, track_id: u32) -> Result<Self, GraphError> {
        let params = profile.encoder_params();
        let channels = usize::from(params.channels);
        let millis = u32::try_from(params.frame_size.as_millis()).unwrap_or(20);
        let samples_per_channel = millis.saturating_mul(48);
        let frame_samples = usize::try_from(samples_per_channel).unwrap_or(960) * channels;
        Ok(Self {
            channels,
            quantum: 480 * channels,
            frame_samples,
            samples_per_channel,
            encoder: Encoder::new(profile)?,
            pending: Vec::with_capacity(frame_samples + 480 * channels),
            frame: vec![0.0; frame_samples],
            seq: 0,
            timestamp: 0,
            track_id,
            gate: false,
            talking: false,
            click_due: false,
            frame_has_click: false,
            quanta: 0,
            measure: false,
            outgoing: Vec::new(),
            tracks: HashMap::new(),
            mixer: Mixer::new(),
            sent_bytes: 0,
            profile,
            snapshot: Snapshot::default(),
        })
    }

    /// Inject a click every 500 ms of capture, on a quantum that starts a frame.
    pub fn set_measure(&mut self, enabled: bool) {
        self.measure = enabled;
    }

    /// Channel count of the capture device for this profile.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Samples in one capture quantum.
    #[must_use]
    pub fn quantum(&self) -> usize {
        self.quantum
    }

    /// Start decoding `id` with this client's profile. The server's advertised
    /// channel count is not consulted; both ends of a spike call pass the same
    /// `--profile`.
    pub fn add_track(&mut self, id: u32) {
        if id == 0 || id == self.track_id || self.tracks.contains_key(&id) {
            return;
        }
        if let Ok(receiver) = TrackReceiver::new(self.profile) {
            self.tracks.insert(id, receiver);
        }
    }

    /// Drop a track after `TrackRemoved`.
    pub fn remove_track(&mut self, id: u32) {
        self.tracks.remove(&id);
    }

    /// Encode available frames from one 10 ms capture quantum.
    ///
    /// `gate` is push-to-talk, voice activation, or open mic. A measurement
    /// click is sent even when the gate is closed. Returns whether this call
    /// injected a click.
    pub fn push_capture(&mut self, input: &[f32], gate: bool) -> Result<bool, GraphError> {
        if input.len() != self.quantum {
            return Err(GraphError::Frame);
        }
        self.gate = gate;
        self.quanta += 1;
        // A 20 ms profile finishes a frame on every second quantum, so a due
        // click waits until the accumulator is empty and the burst starts a packet.
        if self.measure && self.quanta.is_multiple_of(50) {
            self.click_due = true;
        }
        let inject = self.click_due && self.pending.is_empty();
        let start = self.pending.len();
        self.pending.extend_from_slice(input);
        if inject {
            mix_click(&mut self.pending[start..], self.channels);
            self.frame_has_click = true;
            self.click_due = false;
        }
        while self.pending.len() >= self.frame_samples {
            if let Some(packet) = self.finish_frame()? {
                self.outgoing.push(packet);
            }
        }
        Ok(inject)
    }

    /// Take datagrams produced by [`Self::push_capture`].
    pub fn pop_outgoing(&mut self) -> Vec<Bytes> {
        std::mem::take(&mut self.outgoing)
    }

    /// Admit one datagram. Malformed packets and unknown tracks are ignored.
    pub fn push_datagram(&mut self, bytes: &[u8], arrival: Duration) {
        let Ok((header, payload)) = DatagramHeader::parse(bytes) else {
            return;
        };
        if header.track_id == self.track_id {
            return;
        }
        let Some(track) = self.tracks.get_mut(&header.track_id) else {
            return;
        };
        let header = PacketHeader {
            seq: header.seq,
            timestamp: header.timestamp,
            talk_start: header.flags & TALK_START != 0,
            talk_end: header.flags & TALK_END != 0,
        };
        let _ = track.push(header, payload, arrival);
    }

    /// Mix one playback quantum. `now` is the session monotonic clock.
    pub fn render(&mut self, now: Duration) -> StereoFrame {
        let mut lost = 0_u64;
        let mut accepted = 0_u64;
        let mut plc = 0_u64;
        let mut buffer = Duration::ZERO;
        let mut speakers = 0_usize;
        for track in self.tracks.values_mut() {
            let frame = *track.render(now);
            if track.speaking(now) {
                speakers += 1;
            }
            buffer = buffer.max(track.applied_delay());
            let stats = track.stats();
            lost += stats.jitter.lost;
            accepted += stats.jitter.accepted;
            plc += stats.plc;
            let _ = self.mixer.add_track(&frame, 1.0);
        }
        self.snapshot = Snapshot {
            sent_bytes: self.sent_bytes,
            lost,
            accepted,
            plc,
            buffer,
            speakers,
        };
        *self.mixer.finish()
    }

    /// Latest [`Self::render`] counters.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
    }

    fn finish_frame(&mut self) -> Result<Option<Bytes>, GraphError> {
        self.frame
            .copy_from_slice(&self.pending[..self.frame_samples]);
        self.pending.drain(..self.frame_samples);
        let click = self.frame_has_click;
        self.frame_has_click = false;
        let open = self.gate || click;
        let timestamp = self.timestamp;
        self.timestamp = self.timestamp.wrapping_add(self.samples_per_channel);
        if !open && !self.talking {
            return Ok(None);
        }
        let mut flags = 0_u8;
        if !self.talking {
            flags |= TALK_START;
        }
        if !open {
            flags |= TALK_END;
        }
        self.talking = open;
        if self.track_id == 0 {
            return Ok(None);
        }
        let payload = self.encoder.encode(&self.frame)?.to_vec();
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        self.sent_bytes += u64::try_from(payload.len()).unwrap_or(0);
        Ok(Some(audio_packet(
            self.track_id,
            seq,
            timestamp,
            flags,
            &payload,
        )?))
    }
}

/// Build one audio datagram. The payload is copied into the buffer.
pub fn audio_packet(
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

/// Sequence number from a datagram this client built, or none when the header
/// is truncated.
#[must_use]
pub fn packet_seq(bytes: &[u8]) -> Option<u16> {
    DatagramHeader::parse(bytes)
        .ok()
        .map(|(header, _)| header.seq)
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
mod tests {
    use super::*;

    fn tone(frame: &mut [f32], channels: usize, start: usize, frequency: f32, amplitude: f32) {
        for index in 0..frame.len() / channels {
            let time = (start + index) as f64 / 48_000.0;
            let value = (f64::from(amplitude)
                * (2.0 * std::f64::consts::PI * f64::from(frequency) * time).sin())
                as f32;
            for channel in 0..channels {
                frame[index * channels + channel] = value;
            }
        }
    }

    fn energy(frame: &StereoFrame) -> f64 {
        frame.iter().map(|sample| f64::from(*sample).powi(2)).sum()
    }

    #[test]
    fn a_second_graph_hears_the_tone() {
        let mut sender = VoiceGraph::new(VoiceProfile::Voice, 1).unwrap();
        let mut receiver = VoiceGraph::new(VoiceProfile::Voice, 2).unwrap();
        receiver.add_track(1);
        let mut frame = vec![0.0; sender.quantum()];
        let mut arrival = Duration::from_millis(20);
        for index in 0..20_u32 {
            tone(&mut frame, 1, (index as usize) * 480, 440.0, 0.25);
            sender.push_capture(&frame, true).unwrap();
            for packet in sender.pop_outgoing() {
                receiver.push_datagram(&packet, arrival);
                arrival += Duration::from_millis(20);
            }
        }
        let mut total = 0.0;
        for step in 0..80 {
            total += energy(&receiver.render(Duration::from_millis(step * 10)));
        }
        assert!(total > 0.5, "mixed energy {total}");
        assert!(receiver.snapshot().accepted > 0);
    }

    #[test]
    fn five_percent_loss_still_renders_and_counts_gaps() {
        let mut sender = VoiceGraph::new(VoiceProfile::Voice, 1).unwrap();
        let mut receiver = VoiceGraph::new(VoiceProfile::Voice, 2).unwrap();
        receiver.add_track(1);
        let impairment = crate::Impairment {
            loss_percent: 5,
            ..crate::Impairment::default()
        };
        let mut frame = vec![0.0; sender.quantum()];
        let mut arrival = Duration::ZERO;
        for index in 0..100_u32 {
            tone(&mut frame, 1, (index as usize) * 480, 440.0, 0.25);
            sender.push_capture(&frame, true).unwrap();
            for packet in sender.pop_outgoing() {
                let seq = packet_seq(&packet).unwrap();
                if !impairment.drops(seq) {
                    receiver.push_datagram(&packet, arrival);
                }
                arrival += Duration::from_millis(20);
            }
        }
        let mut total = 0.0;
        for step in 0..250 {
            total += energy(&receiver.render(Duration::from_millis(step * 10)));
        }
        assert!(receiver.snapshot().lost >= 1, "plc/loss did not run");
        assert!(total > 0.5, "concealed audio was silent ({total})");
    }
}
