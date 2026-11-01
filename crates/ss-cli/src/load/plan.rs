//! What each synthetic user does and when. Pure functions over a seeded
//! generator so the schedule is reproducible and unit-testable.

use std::time::Duration;

/// Shortest talk burst. A burst always carries at least a few frames.
const MIN_BURST: Duration = Duration::from_millis(200);
/// Shortest pause between two scheduled actions.
const MIN_GAP: Duration = Duration::from_millis(100);

/// Per-bot behaviour shared by every bot in a run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Behavior {
    /// Fraction of time spent talking, `0.0..=1.0`.
    pub duty: f64,
    /// Mean talk-burst length.
    pub burst: Duration,
    /// Text messages per bot per minute. Zero sends none.
    pub text_per_minute: f64,
    /// Mean time in voice between a leave and the next rejoin. `None` stays.
    pub churn: Option<Duration>,
    /// How long a bot stays out of voice after a churn leave.
    pub away: Duration,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            duty: 0.3,
            burst: Duration::from_secs(3),
            text_per_minute: 1.0,
            churn: None,
            away: Duration::from_secs(5),
        }
    }
}

/// A rejected flag value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// Duty cycle outside `0..=1`.
    #[error("duty cycle must be between 0 and 1")]
    Duty,
    /// Zero or negative burst length.
    #[error("burst length must be positive")]
    Burst,
    /// Negative or non-finite text rate.
    #[error("text rate must be zero or positive")]
    TextRate,
}

impl Behavior {
    /// Rejects values the scheduler cannot honour.
    pub fn validate(&self) -> Result<(), PlanError> {
        if !(0.0..=1.0).contains(&self.duty) {
            return Err(PlanError::Duty);
        }
        if self.burst.is_zero() {
            return Err(PlanError::Burst);
        }
        if !self.text_per_minute.is_finite() || self.text_per_minute < 0.0 {
            return Err(PlanError::TextRate);
        }
        Ok(())
    }

    /// Length of the next talk burst. Infinite talk when the duty is 1.
    pub fn next_burst(&self, rng: &mut Rng) -> Duration {
        if self.duty >= 1.0 {
            return Duration::MAX;
        }
        rng.exponential(self.burst)
            .clamp(MIN_BURST, self.burst * 10)
    }

    /// Length of the next silence. Zero duty never talks.
    pub fn next_silence(&self, rng: &mut Rng) -> Duration {
        if self.duty <= 0.0 {
            return Duration::MAX;
        }
        if self.duty >= 1.0 {
            return Duration::ZERO;
        }
        let mean = self.burst.mul_f64((1.0 - self.duty) / self.duty);
        rng.exponential(mean).clamp(MIN_GAP, mean * 10)
    }

    /// Wait until the next text message. `None` when text is off.
    pub fn next_text(&self, rng: &mut Rng) -> Option<Duration> {
        if self.text_per_minute <= 0.0 {
            return None;
        }
        let mean = Duration::from_secs_f64(60.0 / self.text_per_minute);
        Some(rng.exponential(mean).clamp(MIN_GAP, mean * 10))
    }

    /// Time in voice before the next churn leave. `None` when churn is off.
    pub fn next_leave(&self, rng: &mut Rng) -> Option<Duration> {
        self.churn
            .map(|mean| rng.exponential(mean).clamp(MIN_GAP, mean * 10))
    }
}

/// Talk/silence state for one bot, advanced on each 20 ms tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Talk {
    talking: bool,
    left: Duration,
}

/// What the bot sends on this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// Nothing; the bot is silent.
    Silent,
    /// First packet of a burst (`TALK_START`).
    Start,
    /// A packet inside a burst.
    Middle,
    /// Last packet of a burst (`TALK_END`).
    End,
}

impl Talk {
    /// Starts silent for a random fraction of one silence so bots do not
    /// begin their bursts in lockstep.
    pub fn new(behavior: &Behavior, rng: &mut Rng) -> Self {
        let first = behavior.next_silence(rng);
        let left = if first == Duration::MAX {
            first
        } else {
            first.mul_f64(rng.unit())
        };
        Self {
            talking: false,
            left,
        }
    }

    /// Advances by `step` and says what to send for this frame.
    pub fn tick(&mut self, behavior: &Behavior, rng: &mut Rng, step: Duration) -> Frame {
        if self.talking {
            if self.left > step {
                self.left = self.left.saturating_sub(step);
                return Frame::Middle;
            }
            self.talking = false;
            self.left = behavior.next_silence(rng);
            return Frame::End;
        }
        if self.left > step {
            self.left = self.left.saturating_sub(step);
            return Frame::Silent;
        }
        self.talking = true;
        self.left = behavior.next_burst(rng);
        Frame::Start
    }

    /// True while a burst is open.
    #[cfg(test)]
    pub fn talking(&self) -> bool {
        self.talking
    }
}

/// xorshift64* generator. Not cryptographic; seeds come from the bot index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rng(u64);

impl Rng {
    /// A generator for `seed`. Zero is remapped because xorshift sticks at 0.
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    /// Next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        // 53 random bits fill an f64 mantissa exactly.
        let bits = self.next_u64() >> 11;
        #[allow(clippy::cast_precision_loss)]
        let value = bits as f64;
        value / 9_007_199_254_740_992.0
    }

    /// Exponentially distributed with the given mean.
    pub fn exponential(&mut self, mean: Duration) -> Duration {
        let sample = -(1.0 - self.unit()).ln();
        mean.mul_f64(sample.min(50.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEP: Duration = Duration::from_millis(20);

    fn talk_fraction(behavior: &Behavior, seed: u64, ticks: u32) -> f64 {
        let mut rng = Rng::new(seed);
        let mut talk = Talk::new(behavior, &mut rng);
        let mut sent = 0_u32;
        for _ in 0..ticks {
            if talk.tick(behavior, &mut rng, STEP) != Frame::Silent {
                sent += 1;
            }
        }
        f64::from(sent) / f64::from(ticks)
    }

    #[test]
    fn duty_cycle_converges_to_the_configured_fraction() {
        for duty in [0.1, 0.3, 0.5, 0.8] {
            let behavior = Behavior {
                duty,
                ..Behavior::default()
            };
            // Two simulated hours per bot.
            let fraction = talk_fraction(&behavior, 7, 360_000);
            assert!(
                (fraction - duty).abs() < 0.04,
                "duty {duty} produced {fraction}"
            );
        }
    }

    #[test]
    fn full_and_zero_duty_are_constant() {
        let always = Behavior {
            duty: 1.0,
            ..Behavior::default()
        };
        assert!((talk_fraction(&always, 3, 10_000) - 1.0).abs() < 1e-9);
        let never = Behavior {
            duty: 0.0,
            ..Behavior::default()
        };
        assert!(talk_fraction(&never, 3, 10_000).abs() < 1e-9);
    }

    #[test]
    fn every_burst_starts_and_ends_once() {
        let behavior = Behavior::default();
        let mut rng = Rng::new(11);
        let mut talk = Talk::new(&behavior, &mut rng);
        let mut open = false;
        let mut bursts = 0;
        for _ in 0..100_000 {
            match talk.tick(&behavior, &mut rng, STEP) {
                Frame::Start => {
                    assert!(!open, "two starts without an end");
                    open = true;
                    bursts += 1;
                }
                Frame::End => {
                    assert!(open, "end without a start");
                    open = false;
                }
                Frame::Middle => assert!(open),
                Frame::Silent => assert!(!open),
            }
            assert_eq!(talk.talking(), open);
        }
        assert!(bursts > 50, "only {bursts} bursts in 2000 s");
    }

    #[test]
    fn text_interval_matches_the_rate() {
        let behavior = Behavior {
            text_per_minute: 6.0,
            ..Behavior::default()
        };
        let mut rng = Rng::new(5);
        let total: Duration = (0..10_000)
            .map(|_| behavior.next_text(&mut rng).unwrap())
            .sum();
        let mean = total.as_secs_f64() / 10_000.0;
        assert!((mean - 10.0).abs() < 0.6, "mean text gap {mean}");
        let quiet = Behavior {
            text_per_minute: 0.0,
            ..Behavior::default()
        };
        assert_eq!(quiet.next_text(&mut rng), None);
    }

    #[test]
    fn churn_is_off_unless_configured() {
        let mut rng = Rng::new(9);
        assert_eq!(Behavior::default().next_leave(&mut rng), None);
        let churning = Behavior {
            churn: Some(Duration::from_secs(60)),
            ..Behavior::default()
        };
        assert!(churning.next_leave(&mut rng).is_some());
    }

    #[test]
    fn invalid_flags_are_rejected() {
        let mut behavior = Behavior {
            duty: 1.5,
            ..Behavior::default()
        };
        assert_eq!(behavior.validate(), Err(PlanError::Duty));
        behavior.duty = 0.5;
        behavior.burst = Duration::ZERO;
        assert_eq!(behavior.validate(), Err(PlanError::Burst));
        behavior.burst = Duration::from_secs(1);
        behavior.text_per_minute = f64::NAN;
        assert_eq!(behavior.validate(), Err(PlanError::TextRate));
        behavior.text_per_minute = 0.5;
        assert_eq!(behavior.validate(), Ok(()));
    }

    #[test]
    fn generator_is_uniform_enough_and_seeded() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(1);
        assert_eq!(a.next_u64(), b.next_u64());
        let mut rng = Rng::new(0);
        let mean = (0..100_000).map(|_| rng.unit()).sum::<f64>() / 100_000.0;
        assert!((mean - 0.5).abs() < 0.01, "mean {mean}");
    }
}
