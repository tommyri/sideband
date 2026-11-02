//! Bounded, sequence-ordered playout scheduling. All times use the caller's
//! monotonic clock; no network types or clock reads enter the audio layer.

use std::time::Duration;

use ss_core::VoiceProfile;

use crate::codec::{self, MAX_PACKET_BYTES, OpusPacket};

/// Maximum queued packets per track (at least 640 ms at any supported profile).
pub const CAPACITY: usize = 64;
const MIN_DELAY: Duration = Duration::from_millis(20);
const MAX_DELAY: Duration = Duration::from_millis(200);
const INITIAL_DELAY: Duration = Duration::from_millis(40);
const TIMEOUT: Duration = Duration::from_millis(200);
const WINDOW: Duration = Duration::from_secs(2);
const OBSERVATIONS: usize = 200;

/// Audio timing metadata, mapped from the validated wire header by client-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    /// Wrapping 16-bit packet sequence number.
    pub seq: u16,
    /// Wrapping 48 kHz sample timestamp.
    pub timestamp: u32,
    /// First packet of a talk burst. Playout may also infer this from a whole
    /// missing capture frame when sequence numbers show the sender was gated.
    pub talk_start: bool,
    /// Last packet of a talk burst.
    pub talk_end: bool,
}

/// Owned packet storage; construction copies into fixed capacity, never allocates.
#[derive(Clone)]
pub struct Packet {
    /// Audio timing and talk flags.
    pub header: PacketHeader,
    bytes: [u8; MAX_PACKET_BYTES],
    length: usize,
    arrival: Duration,
}

impl Packet {
    /// Encoded Opus payload.
    pub fn payload(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

/// Rejected input or unsupported stream configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Codec framing error.
    #[error(transparent)]
    Codec(#[from] codec::Error),
    /// Frame duration does not match the configured profile.
    #[error("packet duration differs from track profile")]
    Duration,
    /// Queue is full or packet is outside the 64-slot sequence window.
    #[error("jitter queue capacity or sequence window exceeded")]
    Capacity,
}

/// Result of admission; late packets inform timing but never replace committed audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Queued for playout.
    Queued,
    /// Slot was already committed to audio (original or concealment).
    Late,
    /// Same sequence number is already queued.
    Duplicate,
}

/// Scheduling counters, independent of decoder recovery choices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Packets accepted into the queue.
    pub accepted: u64,
    /// Packets arriving after their slots were committed.
    pub late: u64,
    /// Duplicate packets still present in the queue.
    pub duplicate: u64,
    /// Missing source slots committed for recovery.
    pub lost: u64,
    /// Capacity/window rejections.
    pub overflow: u64,
    /// Concealment frames inserted to increase playout delay.
    pub expanded: u64,
    /// Decoded frames skipped to decrease playout delay.
    pub contracted: u64,
    /// Playout worker missed its deadline by at least 200 ms.
    pub underruns: u64,
}

/// One scheduling decision. Packet storage is moved/copied without heap allocation.
#[allow(clippy::large_enum_variant)] // Intentional fixed storage on the playback worker.
pub enum Playout {
    /// A source packet is ready; optionally decode and discard its predecessor
    /// first to preserve predictive codec state during silence contraction.
    Packet {
        /// Packet to decode into output.
        packet: Packet,
        /// Packet to decode without rendering.
        discarded: Option<Packet>,
    },
    /// Recover a missing slot or insert concealment for delay expansion.
    Loss {
        /// Nearest future packet, retained in the queue for normal decode later.
        following: Option<Packet>,
        /// Distance to that packet; one permits in-band FEC, larger gaps DRED.
        missing: usize,
        /// This is an inserted frame; source sequence must not advance.
        expanded: bool,
    },
    /// Startup, inter-burst silence, or the next deadline has not arrived.
    Wait,
}

#[derive(Clone, Copy, Default)]
struct Observation {
    at: Duration,
    deviation: Duration,
}

/// Fixed-capacity jitter buffer for one profile and one ordered sequence space.
/// Construct once per track; a sender sequence reset requires a new track.
pub struct JitterBuffer {
    queue: Vec<Packet>,
    frame: Duration,
    expected: Option<u16>,
    committed: Option<u16>,
    deadline: Duration,
    playing: bool,
    started: bool,
    burst_start: Option<u16>,
    last_arrival: Duration,
    newest: Option<(PacketHeader, Duration)>,
    observations: [Observation; OBSERVATIONS],
    observation_count: usize,
    observation_cursor: usize,
    minimum: Duration,
    maximum: Duration,
    target: Duration,
    applied: Duration,
    silent: bool,
    stats: Stats,
}

pub(crate) fn sequence_delta(a: u16, b: u16) -> i16 {
    i16::from_ne_bytes(a.wrapping_sub(b).to_ne_bytes())
}

impl JitterBuffer {
    /// Allocate packet capacity and start with 40 ms of buffering.
    pub fn new(profile: VoiceProfile) -> Self {
        Self {
            queue: Vec::with_capacity(CAPACITY),
            frame: profile.encoder_params().frame_size,
            expected: None,
            committed: None,
            deadline: Duration::ZERO,
            playing: false,
            started: false,
            burst_start: None,
            last_arrival: Duration::ZERO,
            newest: None,
            observations: [Observation::default(); OBSERVATIONS],
            observation_count: 0,
            observation_cursor: 0,
            minimum: MIN_DELAY,
            maximum: MAX_DELAY,
            target: INITIAL_DELAY,
            applied: INITIAL_DELAY,
            silent: false,
            stats: Stats::default(),
        }
    }

    /// Set validated adaptive buffering bounds. Playout adjusts at silence boundaries.
    pub fn set_bounds(&mut self, minimum: Duration, maximum: Duration) {
        if minimum >= MIN_DELAY && maximum <= MAX_DELAY && minimum <= maximum {
            self.minimum = minimum;
            self.maximum = maximum;
            self.target = self.target.clamp(minimum, maximum);
        }
    }

    /// Counters accumulated for this track.
    pub fn stats(&self) -> Stats {
        self.stats
    }
    /// Estimated target; applied only at a burst boundary or in decoded silence.
    pub fn target_delay(&self) -> Duration {
        self.target
    }
    /// Currently applied buffering delay, excluding network transit.
    pub fn applied_delay(&self) -> Duration {
        self.applied
    }
    /// Number of source packets currently buffered.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }
    /// Inform adaptation using decoded PCM before user gain, not muted output.
    pub fn set_decoded_silence(&mut self, silent: bool) {
        self.silent = silent;
    }

    /// Validate and copy a packet using its local monotonic arrival time.
    pub fn push(
        &mut self,
        mut header: PacketHeader,
        payload: &[u8],
        arrival: Duration,
    ) -> Result<Admission, Error> {
        if OpusPacket::toc(payload)?.duration() != self.frame {
            return Err(Error::Duration);
        }
        // If both burst flags were lost, a whole-frame gap in the capture
        // clock (beyond the sequence distance) still identifies gated silence.
        // Timestamp and sequence differences use their wrapping wire widths.
        if let Some((previous, _)) = self.newest {
            let distance = sequence_delta(header.seq, previous.seq);
            let samples = header.timestamp.wrapping_sub(previous.timestamp);
            let expected_samples =
                u128::from(distance.unsigned_abs()) * self.frame.as_millis() * 48;
            if distance > 0
                && samples < (1 << 31)
                && u128::from(samples) >= expected_samples + self.frame.as_millis() * 48
            {
                header.talk_start = true;
            }
        }
        // A lost TALK_END can make PLC advance past sequence numbers that the
        // gated sender never transmitted. A genuinely newer TALK_START rebases
        // those speculative slots; an old/reordered START cannot rewind audio.
        if header.talk_start
            && self
                .newest
                .is_some_and(|(previous, _)| sequence_delta(header.seq, previous.seq) > 0)
            && self
                .committed
                .is_some_and(|seq| sequence_delta(header.seq, seq) <= 0)
        {
            self.queue.clear();
            self.committed = Some(header.seq.wrapping_sub(1));
            self.playing = false;
            self.started = false;
        }
        if self
            .committed
            .is_some_and(|seq| sequence_delta(header.seq, seq) <= 0)
        {
            // A rising network delay must remain observable even after the
            // original audio slot was concealed. Only forward observations count.
            self.observe(header, arrival);
            self.stats.late += 1;
            return Ok(Admission::Late);
        }
        if self.queue.iter().any(|p| p.header.seq == header.seq) {
            self.stats.duplicate += 1;
            return Ok(Admission::Duplicate);
        }
        let delta = self
            .expected
            .map_or(0, |seq| sequence_delta(header.seq, seq));
        let outside = self.playing
            && (usize::from(delta.unsigned_abs()) >= CAPACITY)
            && !(header.talk_start && delta > 0);
        let widens_startup_window = !self.started
            && delta < 0
            && self.queue.last().is_some_and(|last| {
                usize::from(last.header.seq.wrapping_sub(header.seq)) >= CAPACITY
            });
        if self.queue.len() == CAPACITY || outside || widens_startup_window {
            self.stats.overflow += 1;
            return Err(Error::Capacity);
        }
        if self.started && delta < 0 {
            self.stats.late += 1;
            return Ok(Admission::Late);
        }
        if !self.playing {
            self.start_burst(header.seq, arrival);
        } else if !self.started && delta < 0 {
            // Reordering before the first release may reveal an earlier packet.
            self.expected = Some(header.seq);
            self.burst_start = Some(header.seq);
            self.deadline = self
                .deadline
                .saturating_sub(self.frame * u32::from(delta.unsigned_abs()));
        }
        self.observe(header, arrival);
        let mut packet = Packet {
            header,
            bytes: [0; MAX_PACKET_BYTES],
            length: payload.len(),
            arrival,
        };
        packet.bytes[..payload.len()].copy_from_slice(payload);
        let index = self
            .queue
            .iter()
            .position(|p| sequence_delta(header.seq, p.header.seq) < 0)
            .unwrap_or(self.queue.len());
        self.queue.insert(index, packet);
        self.last_arrival = self.last_arrival.max(arrival);
        self.stats.accepted += 1;
        Ok(Admission::Queued)
    }

    fn start_burst(&mut self, seq: u16, arrival: Duration) {
        // A long pause must not retain a stale high-delay estimate forever.
        self.estimate(arrival);
        self.expected = Some(seq);
        self.burst_start = Some(seq);
        self.deadline = arrival.saturating_add(self.target);
        self.applied = self.target;
        self.playing = true;
        self.started = false;
        self.silent = false;
    }

    fn observe(&mut self, header: PacketHeader, arrival: Duration) {
        if let Some((previous, at)) = self.newest {
            if sequence_delta(header.seq, previous.seq) <= 0 {
                return;
            }
            let samples = header.timestamp.wrapping_sub(previous.timestamp);
            if !header.talk_start && samples <= 96_000 && arrival >= at {
                let sent = Duration::from_micros(u64::from(samples) * 1_000_000 / 48_000);
                let received = arrival.saturating_sub(at);
                let deviation = received.abs_diff(sent);
                self.observations[self.observation_cursor] = Observation {
                    at: arrival,
                    deviation,
                };
                self.observation_cursor = (self.observation_cursor + 1) % OBSERVATIONS;
                self.observation_count = (self.observation_count + 1).min(OBSERVATIONS);
                self.estimate(arrival);
            }
        }
        self.newest = Some((header, arrival));
    }

    fn estimate(&mut self, now: Duration) {
        let mut deviations = [Duration::ZERO; OBSERVATIONS];
        let mut count = 0;
        for observation in &self.observations[..self.observation_count] {
            if now.saturating_sub(observation.at) <= WINDOW {
                deviations[count] = observation.deviation;
                count += 1;
            }
        }
        if count == 0 {
            self.target = INITIAL_DELAY.clamp(self.minimum, self.maximum);
            return;
        }
        deviations[..count].sort_unstable();
        let percentile = deviations[(count * 95).div_ceil(100) - 1];
        let milliseconds = (self.minimum + percentile).min(self.maximum).as_millis();
        let frame_ms = self.frame.as_millis();
        let rounded = milliseconds.div_ceil(frame_ms) * frame_ms;
        self.target = Duration::from_millis(u64::try_from(rounded).unwrap_or(200))
            .clamp(self.minimum, self.maximum);
    }

    fn advance(&mut self) {
        if let Some(seq) = self.expected {
            self.committed = Some(seq);
            self.expected = Some(seq.wrapping_add(1));
        }
        self.started = true;
    }

    /// Reserve additional slots already recovered by DRED into receiver storage.
    /// They can no longer be replaced by arriving originals after decoder commit.
    #[cfg(feature = "dred")]
    pub(crate) fn reserve_losses(&mut self, count: usize) {
        for _ in 0..count {
            self.advance();
            self.deadline = self.deadline.saturating_add(self.frame);
            self.stats.lost += 1;
        }
    }

    /// Release one frame at its deadline. Missing audio becomes a recovery
    /// decision, not an underrun. Call on the playback worker at 10 ms cadence.
    pub fn pop_for_playout(&mut self, now: Duration) -> Playout {
        if !self.playing {
            if let Some(first) = self.queue.first() {
                self.start_burst(first.header.seq, first.arrival);
            } else {
                return Playout::Wait;
            }
        }
        if let Some(first) = self.queue.first()
            && first.header.talk_start
            && self.burst_start != Some(first.header.seq)
        {
            self.start_burst(first.header.seq, first.arrival);
        }
        if now < self.deadline {
            return Playout::Wait;
        }
        if now.saturating_sub(self.deadline) >= TIMEOUT {
            self.stats.underruns += 1;
            if let Some(last) = self.queue.last() {
                self.committed = Some(last.header.seq);
                self.expected = Some(last.header.seq.wrapping_add(1));
            }
            self.queue.clear();
            self.playing = false;
            return Playout::Wait;
        }
        if self.queue.is_empty() && now.saturating_sub(self.last_arrival) >= TIMEOUT {
            self.playing = false;
            return Playout::Wait;
        }
        if self.silent && self.target > self.applied {
            self.applied += self.frame;
            self.deadline = self.deadline.saturating_add(self.frame);
            self.stats.expanded += 1;
            return Playout::Loss {
                following: None,
                missing: 0,
                expanded: true,
            };
        }
        let mut discarded = None;
        if self.silent
            && self.target < self.applied
            && self.queue.len() >= 2
            && Some(self.queue[0].header.seq) == self.expected
            && self.queue[1].header.seq == self.queue[0].header.seq.wrapping_add(1)
            && !self.queue[0].header.talk_start
            && !self.queue[0].header.talk_end
            && !self.queue[1].header.talk_start
        {
            discarded = Some(self.queue.remove(0));
            self.advance();
            self.applied -= self.frame;
            self.stats.contracted += 1;
        }
        let expected = self.expected;
        self.advance();
        self.deadline = self.deadline.saturating_add(self.frame);
        if self
            .queue
            .first()
            .is_some_and(|p| Some(p.header.seq) == expected)
        {
            let packet = self.queue.remove(0);
            if packet.header.talk_end {
                self.playing = false;
            }
            Playout::Packet { packet, discarded }
        } else {
            self.stats.lost += 1;
            let following = self.queue.first().cloned();
            let missing = following
                .as_ref()
                .zip(expected)
                .map_or(1, |(p, seq)| usize::from(p.header.seq.wrapping_sub(seq)));
            Playout::Loss {
                following,
                missing,
                expanded: false,
            }
        }
    }
}
