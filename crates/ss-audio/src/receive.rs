//! One jitter buffer and predictive decoder per incoming track. Call `render`
//! once per 10 ms on the playback worker and mix its stereo output.

use std::time::Duration;

use ss_core::VoiceProfile;

use crate::{
    codec::{self, Decoder},
    jitter::{self, Admission, JitterBuffer, PacketHeader, Playout, sequence_delta},
    mixer::{self, SAMPLES_PER_CHANNEL, StereoFrame},
};

/// Receiver recovery counters. Counts are source codec frames (10 or 20 ms),
/// except `waiting_quanta`, which counts silent 10 ms output quanta.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Queue, loss, and adaptation counters.
    pub jitter: jitter::Stats,
    /// Explicit calls to decoder PLC, including delay expansion.
    pub plc: u64,
    /// In-band FEC requests; libopus may internally fall back to PLC.
    pub fec_attempts: u64,
    /// Frames recovered using DRED history.
    pub dred: u64,
    /// Bad encoded audio or other decoder failures.
    pub decode_errors: u64,
    /// Startup/inter-burst output quanta for which no audio is due.
    pub waiting_quanta: u64,
}

/// Per-track receiver with reusable decoded storage, gain, and speaking state.
/// Network ownership and user identity live in client-core, outside this type.
pub struct TrackReceiver {
    jitter: JitterBuffer,
    decoder: Decoder,
    channels: usize,
    frame_samples: usize,
    pending: Box<[f32]>,
    length: usize,
    cursor: usize,
    output: StereoFrame,
    gain: f32,
    newest: Option<(u16, Duration, bool)>,
    stats: Stats,
}

impl TrackReceiver {
    /// Allocate and initialize a track off the playback/device threads.
    pub fn new(profile: VoiceProfile) -> Result<Self, codec::Error> {
        let params = profile.encoder_params();
        let frame_samples = usize::try_from(params.frame_size.as_millis())
            .map_err(|_| codec::Error::Configuration)?
            * 48;
        #[cfg(feature = "dred")]
        let capacity = 48_000 * 2;
        #[cfg(not(feature = "dred"))]
        let capacity = frame_samples * 2;
        Ok(Self {
            jitter: JitterBuffer::new(profile),
            decoder: Decoder::for_profile(profile)?,
            channels: usize::from(params.channels),
            frame_samples,
            pending: vec![0.0; capacity].into_boxed_slice(),
            length: 0,
            cursor: 0,
            output: [0.0; SAMPLES_PER_CHANNEL * 2],
            gain: 1.0,
            newest: None,
            stats: Stats::default(),
        })
    }

    /// Admit already-routed audio. Duplicate/late packets cannot refresh the
    /// speaking timer or replay a burst flag.
    pub fn push(
        &mut self,
        header: PacketHeader,
        payload: &[u8],
        arrival: Duration,
    ) -> Result<Admission, jitter::Error> {
        let admission = self.jitter.push(header, payload, arrival)?;
        if admission == Admission::Queued
            && self
                .newest
                .is_none_or(|(seq, _, _)| sequence_delta(header.seq, seq) > 0)
        {
            self.newest = Some((header.seq, arrival, !header.talk_end));
        }
        Ok(admission)
    }

    /// Speaking follows newest admitted flags and expires after 200 ms without
    /// a newer packet, including when TALK_START or TALK_END is lost. This is
    /// a UI indicator, not a mixing gate: keep rendering to drain queued tails.
    pub fn speaking(&self, now: Duration) -> bool {
        self.newest.is_some_and(|(_, at, talking)| {
            talking && now.saturating_sub(at) < Duration::from_millis(200)
        })
    }

    /// Per-track linear gain (0 through 4), applied after silence detection.
    pub fn set_gain(&mut self, gain: f32) -> Result<(), mixer::Error> {
        mixer::validate_gain(gain)?;
        self.gain = gain;
        Ok(())
    }

    /// Snapshot of scheduling and decoding counters.
    pub fn stats(&self) -> Stats {
        Stats {
            jitter: self.jitter.stats(),
            ..self.stats
        }
    }
    /// Estimated adaptive delay, excluding network transit.
    pub fn target_delay(&self) -> Duration {
        self.jitter.target_delay()
    }
    /// Applied adaptive delay, excluding network transit.
    pub fn applied_delay(&self) -> Duration {
        self.jitter.applied_delay()
    }

    fn plc(&mut self) {
        self.stats.plc += 1;
        if let Ok(pcm) = self.decoder.decode(None, false) {
            self.length = to_stereo(pcm, self.channels, &mut self.pending);
        } else {
            self.stats.decode_errors += 1;
            self.length = self.frame_samples * 2;
            self.pending[..self.length].fill(0.0);
        }
    }

    fn decode(&mut self, payload: &[u8], fec: bool) {
        if let Ok(pcm) = self.decoder.decode(Some(payload), fec) {
            self.length = to_stereo(pcm, self.channels, &mut self.pending);
        } else {
            self.stats.decode_errors += 1;
            self.plc();
        }
    }

    fn refill(&mut self, now: Duration) {
        self.cursor = 0;
        self.length = 0;
        match self.jitter.pop_for_playout(now) {
            Playout::Wait => {}
            Playout::Packet { packet, discarded } => {
                if let Some(discarded) = discarded {
                    // Preserve prediction history even when silence is shortened.
                    self.decode(discarded.payload(), false);
                }
                self.decode(packet.payload(), false);
            }
            Playout::Loss {
                following,
                missing,
                expanded,
            } => {
                #[cfg(feature = "dred")]
                if !expanded
                    && missing > 1
                    && missing * self.frame_samples <= 48_000
                    && let Some(packet) = &following
                    && let Ok(pcm) = self.decoder.decode_dred(packet.payload(), missing)
                {
                    self.length = to_stereo(pcm, self.channels, &mut self.pending);
                    self.jitter.reserve_losses(missing - 1);
                    self.stats.dred += u64::try_from(missing).unwrap_or(0);
                    return;
                }
                if !expanded
                    && missing == 1
                    && let Some(packet) = following
                {
                    self.stats.fec_attempts += 1;
                    self.decode(packet.payload(), true);
                } else {
                    self.plc();
                }
            }
        }
    }

    /// Emit exactly 480 stereo samples per channel. Call once every 10 ms using
    /// the same clock as packet arrival times. No allocation, locks, or logging.
    /// Startup and gated silence produce zeros; network loss uses FEC/DRED/PLC.
    pub fn render(&mut self, now: Duration) -> &StereoFrame {
        if self.cursor == self.length {
            self.refill(now);
        }
        if self.length == 0 {
            self.output.fill(0.0);
            self.stats.waiting_quanta += 1;
            self.jitter.set_decoded_silence(false);
        } else {
            let frame = &self.pending[self.cursor..self.cursor + self.output.len()];
            self.jitter
                .set_decoded_silence(frame.iter().all(|s| s.abs() <= 0.001));
            for (output, sample) in self.output.iter_mut().zip(frame) {
                *output = *sample * self.gain;
            }
            self.cursor += self.output.len();
        }
        &self.output
    }
}

fn to_stereo(pcm: &[f32], channels: usize, output: &mut [f32]) -> usize {
    if channels == 2 {
        output[..pcm.len()].copy_from_slice(pcm);
        pcm.len()
    } else {
        for (sample, pair) in pcm.iter().zip(output.as_chunks_mut::<2>().0) {
            *pair = [*sample; 2];
        }
        pcm.len() * 2
    }
}
