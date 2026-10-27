//! Mouth-to-ear measurement. The sender replaces one capture quantum with a
//! known 1 kHz burst. The receiver cross-correlates that burst in the mixed
//! playback audio and adds the capture and playback device delays.

use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};

use ss_audio::mixer::{SAMPLES_PER_CHANNEL, StereoFrame};

const CLICK_SAMPLES: usize = SAMPLES_PER_CHANNEL;
const HISTORY_SAMPLES: usize = 48_000 / 2;
const SEARCH_EVERY: u64 = 5;
/// Ignore another peak this soon after a hit. Clicks are 500 ms apart.
const REFRACTORY: Duration = Duration::from_millis(300);
const SCORE_THRESHOLD: f32 = 0.45;

/// One correlated click, including device buffer delays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencySample {
    /// Capture delay plus network and playout plus playback delay.
    pub total: Duration,
    /// Time from the capture callback to the send-path injection.
    pub capture: Duration,
    /// Time from the rendered sample to the playback callback.
    pub playback: Duration,
}

/// Shared click marks. The sender records an injection; the receiver records
/// the correlated playback instant.
#[derive(Debug, Default)]
pub struct Probe {
    pending: Mutex<VecDeque<Mark>>,
    samples: Mutex<Vec<LatencySample>>,
}

#[derive(Debug, Clone, Copy)]
struct Mark {
    sent: Instant,
    capture: Duration,
}

impl Probe {
    /// Empty probe.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a click entered the encoder at `sent`, already delayed by
    /// `capture` inside the input device.
    pub fn mark(&self, capture: Duration) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(Mark {
                sent: Instant::now(),
                capture,
            });
    }

    /// Pair `heard` with the oldest unmatched click. Returns the sample when
    /// the gap is inside one click period.
    pub fn hear(&self, heard: Instant, playback: Duration) -> Option<LatencySample> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A missed detection must not pair this click with an older mark.
        // Keep the newest mark that started at or before `heard`.
        let mut chosen = None;
        while pending.front().is_some_and(|mark| mark.sent <= heard) {
            chosen = pending.pop_front();
        }
        drop(pending);
        let mark = chosen?;
        let total = heard.saturating_duration_since(mark.sent) + playback + mark.capture;
        if total > Duration::from_millis(450) {
            return None;
        }
        let sample = LatencySample {
            total,
            capture: mark.capture,
            playback,
        };
        self.samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(sample);
        Some(sample)
    }

    /// Totals collected so far, oldest first.
    #[must_use]
    pub fn totals(&self) -> Vec<Duration> {
        self.samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|sample| sample.total)
            .collect()
    }
}

/// Median of a non-empty list. Even lengths average the two central values.
#[must_use]
pub fn median(samples: &[Duration]) -> Option<Duration> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    Some(if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2
    } else {
        sorted[mid]
    })
}

/// The burst written into the send path and sought in the receive path.
#[must_use]
pub fn click() -> &'static [f32] {
    static CLICK: std::sync::LazyLock<Vec<f32>> = std::sync::LazyLock::new(|| {
        (0..CLICK_SAMPLES)
            .map(|index| {
                let position = f64::from(u32::try_from(index).unwrap_or(u32::MAX)) / 48_000.0;
                let window = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * position / 0.010).cos();
                let tone = (2.0 * std::f64::consts::PI * 1_000.0 * position).sin();
                f32_from_f64(0.6 * window * tone)
            })
            .collect()
    });
    &CLICK
}

/// Lag of `reference` inside `signal`, plus the normalized correlation score.
#[must_use]
pub fn find_click(signal: &[f32], reference: &[f32]) -> Option<(usize, f32)> {
    if reference.is_empty() || signal.len() < reference.len() {
        return None;
    }
    let reference_energy = energy(reference);
    if reference_energy <= f64::MIN_POSITIVE {
        return None;
    }
    let last = signal.len() - reference.len();
    let mut coarse = 0_usize;
    let mut coarse_score = 0.0_f32;
    for lag in (0..=last).step_by(8) {
        let score = normalized(signal, reference, lag, reference_energy);
        if score > coarse_score {
            coarse_score = score;
            coarse = lag;
        }
    }
    let mut best_score = coarse_score;
    let mut best_lag = coarse;
    let refine_end = coarse.saturating_add(8).min(last);
    for lag in coarse.saturating_sub(8)..=refine_end {
        let score = normalized(signal, reference, lag, reference_energy);
        if score > best_score {
            best_score = score;
            best_lag = lag;
        }
    }
    (best_score >= SCORE_THRESHOLD).then_some((best_lag, best_score))
}

fn normalized(signal: &[f32], reference: &[f32], lag: usize, reference_energy: f64) -> f32 {
    let mut dot = 0.0_f64;
    let mut signal_energy = 0.0_f64;
    for (offset, sample) in reference.iter().enumerate() {
        let value = f64::from(signal[lag + offset]);
        dot += value * f64::from(*sample);
        signal_energy += value * value;
    }
    let denom = (signal_energy * reference_energy).sqrt();
    if denom <= f64::MIN_POSITIVE {
        0.0
    } else {
        f32_from_f64(dot / denom)
    }
}

fn energy(samples: &[f32]) -> f64 {
    samples
        .iter()
        .map(|sample| {
            let value = f64::from(*sample);
            value * value
        })
        .sum()
}

fn f32_from_f64(value: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    {
        value as f32
    }
}

/// Rolling correlation against [`click`].
#[derive(Debug)]
pub struct Correlator {
    history: VecDeque<f32>,
    origin: Instant,
    dropped: u64,
    frames: u64,
    last_hit: Option<Instant>,
    started: bool,
}

impl Correlator {
    /// Start the sample clock at the first pushed frame.
    #[must_use]
    pub fn new() -> Self {
        Self {
            history: VecDeque::with_capacity(HISTORY_SAMPLES),
            origin: Instant::now(),
            dropped: 0,
            frames: 0,
            last_hit: None,
            started: false,
        }
    }

    /// Push one stereo quantum that ends at `frame_end`. Returns the instant of
    /// a newly correlated click.
    pub fn push(&mut self, frame: &StereoFrame, frame_end: Instant) -> Option<Instant> {
        if !self.started {
            self.origin = frame_end
                .checked_sub(Duration::from_millis(10))
                .unwrap_or(frame_end);
            self.started = true;
        }
        for pair in frame.as_chunks::<2>().0 {
            self.history.push_back(f32::midpoint(pair[0], pair[1]));
        }
        while self.history.len() > HISTORY_SAMPLES {
            self.history.pop_front();
            self.dropped += 1;
        }
        self.frames += 1;
        if !self.frames.is_multiple_of(SEARCH_EVERY) {
            return None;
        }
        // Recent audio only. A longer window makes a debug build miss the
        // 10 ms render deadline and also admits stale false peaks.
        let signal: Vec<f32> = self.history.iter().rev().take(9_600).copied().collect();
        // `rev` walks newest-first. Put the window back in time order.
        let signal: Vec<f32> = signal.into_iter().rev().collect();
        let skipped = self.history.len().saturating_sub(signal.len());
        let (lag, _) = find_click(&signal, click())?;
        let lag = skipped + lag;
        let heard = self.origin + sample_duration(self.dropped + u64::try_from(lag).unwrap_or(0));
        if self
            .last_hit
            .is_some_and(|previous| heard.saturating_duration_since(previous) < REFRACTORY)
        {
            return None;
        }
        self.last_hit = Some(heard);
        Some(heard)
    }
}

fn sample_duration(samples: u64) -> Duration {
    Duration::from_nanos(samples.saturating_mul(1_000_000_000) / 48_000)
}

/// Replace the start of an interleaved frame with the click on every channel.
pub fn mix_click(frame: &mut [f32], channels: usize) {
    if channels == 0 {
        return;
    }
    for (index, sample) in click().iter().enumerate() {
        let base = index * channels;
        if base + channels > frame.len() {
            break;
        }
        for channel in 0..channels {
            frame[base + channel] = *sample;
        }
    }
}

/// Format a latency sample for the terminal.
#[must_use]
pub fn format_sample(sample: LatencySample) -> String {
    format!(
        "mouth-to-ear {} ms (capture {} ms, playback {} ms)",
        millis(sample.total),
        millis(sample.capture),
        millis(sample.playback)
    )
}

/// One decimal millisecond.
#[must_use]
pub fn millis(duration: Duration) -> String {
    format!("{:.1}", duration.as_secs_f64() * 1_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_round_trip_keeps_the_click_detectable() {
        use ss_audio::codec::{Decoder, Encoder};
        use ss_core::VoiceProfile;
        let mut encoder = Encoder::new(VoiceProfile::Voice).unwrap();
        let mut decoder = Decoder::for_profile(VoiceProfile::Voice).unwrap();
        let mut pcm = vec![0.0; encoder.input_samples()];
        let warm = encoder.encode(&pcm).unwrap().to_vec();
        pcm[..click().len()].copy_from_slice(click());
        let clicked = encoder.encode(&pcm).unwrap().to_vec();
        pcm.fill(0.0);
        let after = encoder.encode(&pcm).unwrap().to_vec();
        let mut recovered = Vec::new();
        for packet in [warm, clicked, after] {
            recovered.extend_from_slice(decoder.decode(Some(&packet), false).unwrap());
        }
        assert!(
            find_click(&recovered, click()).is_some(),
            "decoded click was not recognized"
        );
    }

    #[test]
    fn finds_a_delayed_copy_of_the_click() {
        let mut signal = vec![0.0; 2_000];
        let lag = 321;
        signal[lag..lag + click().len()].copy_from_slice(click());
        assert_eq!(
            find_click(&signal, click()).map(|(found, _)| found),
            Some(lag)
        );
    }

    #[test]
    fn median_averages_the_center_pair() {
        let samples = [
            Duration::from_millis(30),
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(40),
        ];
        assert_eq!(median(&samples), Some(Duration::from_millis(25)));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn probe_adds_device_delays() {
        let probe = Probe::new();
        probe.mark(Duration::from_millis(13));
        let sample = probe
            .hear(
                Instant::now() + Duration::from_millis(40),
                Duration::from_millis(11),
            )
            .unwrap();
        assert!(sample.total >= Duration::from_millis(64));
        assert!(sample.total < Duration::from_millis(80));
        assert_eq!(sample.capture, Duration::from_millis(13));
        assert_eq!(sample.playback, Duration::from_millis(11));
    }
}
