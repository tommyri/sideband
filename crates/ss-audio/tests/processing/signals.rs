//! Deterministic synthetic fixtures for the processing chain: formant speech,
//! fan noise, and a sparse room echo. Shared by the tests and the benchmark.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    dead_code
)] // Sample indices and seeds stay far below f32/usize limits.

use std::f32::consts::{PI, TAU};

/// Graph rate in samples per second.
pub const RATE: usize = 48_000;
/// Samples per mono 10 ms frame.
pub const FRAME: usize = 480;

/// xorshift64*: small, deterministic, good enough for noise.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [-1, 1).
    pub fn signed(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1_u64 << 23) as f32 - 1.0
    }

    /// Uniform in [low, high).
    pub fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * 0.5 * (self.signed() + 1.0)
    }
}

/// Two-pole resonator at `frequency` with `bandwidth`, unity-ish peak gain.
struct Resonator {
    a1: f32,
    a2: f32,
    gain: f32,
    y1: f32,
    y2: f32,
}

impl Resonator {
    fn new(frequency: f32, bandwidth: f32) -> Self {
        let r = (-PI * bandwidth / RATE as f32).exp();
        Self {
            a1: 2.0 * r * (TAU * frequency / RATE as f32).cos(),
            a2: -r * r,
            gain: 1.0 - r,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn tick(&mut self, input: f32) -> f32 {
        let output = self.gain * input + self.a1 * self.y1 + self.a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = output;
        output
    }
}

/// Clean speech and a per-sample activity mask.
pub struct Speech {
    pub samples: Vec<f32>,
    pub active: Vec<bool>,
}

const VOWELS: [[f32; 3]; 5] = [
    [730.0, 1090.0, 2440.0],
    [270.0, 2290.0, 3010.0],
    [300.0, 870.0, 2240.0],
    [530.0, 1840.0, 2480.0],
    [570.0, 840.0, 2410.0],
];

/// Syllables of a glottal pulse train through three formants, with pitch
/// movement, fricative onsets, and pauses. RMS over active samples is `rms`.
pub fn speech(seconds: f32, seed: u64, rms: f32) -> Speech {
    let total = (seconds * RATE as f32) as usize;
    let mut rng = Rng::new(seed);
    let mut samples = vec![0.0_f32; total];
    let mut active = vec![false; total];
    let mut position = (rng.range(0.05, 0.2) * RATE as f32) as usize;
    let mut phase = 0.0_f32;
    let mut syllable = 0;
    while position < total {
        let length = (rng.range(0.12, 0.28) * RATE as f32) as usize;
        let vowel = VOWELS[(rng.next_u64() % 5) as usize];
        let shift = rng.range(0.9, 1.1);
        let mut formants: Vec<Resonator> = vowel
            .iter()
            .zip([80.0, 110.0, 150.0])
            .map(|(frequency, bandwidth)| Resonator::new(frequency * shift, bandwidth))
            .collect();
        let pitch = rng.range(105.0, 150.0);
        let fricative = if rng.signed() > 0.3 {
            (0.06 * RATE as f32) as usize
        } else {
            0
        };
        let mut hiss = Resonator::new(5500.0, 3000.0);
        let mut tilt = 0.0_f32;
        let attack = (0.025 * RATE as f32) as usize;
        for index in 0..fricative + length {
            let at = position + index;
            if at >= total {
                break;
            }
            let value = if index < fricative {
                let window = (PI * index as f32 / fricative as f32).sin();
                0.35 * window * hiss.tick(rng.signed())
            } else {
                let voiced = index - fricative;
                let time = at as f32 / RATE as f32;
                let f0 = pitch
                    * (1.0 + 0.08 * (TAU * 0.7 * time).sin())
                    * (1.0 - 0.15 * voiced as f32 / length as f32);
                phase += f0 / RATE as f32;
                let pulse = if phase >= 1.0 {
                    phase -= 1.0;
                    1.0
                } else {
                    0.0
                };
                tilt = 0.9f32.mul_add(tilt, pulse + 0.02 * rng.signed());
                let envelope = if voiced < attack {
                    0.5 - 0.5 * (PI * voiced as f32 / attack as f32).cos()
                } else if voiced + attack > length {
                    0.5 - 0.5 * (PI * (length - voiced) as f32 / attack as f32).cos()
                } else {
                    1.0
                };
                let out: f32 = [1.0, 0.6, 0.3]
                    .iter()
                    .zip(&mut formants)
                    .map(|(weight, formant)| weight * formant.tick(tilt))
                    .sum();
                envelope * out
            };
            samples[at] = value;
            active[at] = true;
        }
        position += fricative + length;
        syllable += 1;
        let pause = if syllable % 5 == 0 {
            rng.range(0.35, 0.6)
        } else {
            rng.range(0.04, 0.15)
        };
        position += (pause * RATE as f32) as usize;
    }
    let energy: f32 = samples
        .iter()
        .zip(&active)
        .filter(|(_, active)| **active)
        .map(|(sample, _)| sample * sample)
        .sum();
    let count = active.iter().filter(|active| **active).count().max(1);
    let scale = rms / (energy / count as f32).sqrt();
    for sample in &mut samples {
        *sample *= scale;
    }
    Speech { samples, active }
}

// Paul Kellet's pink-noise filter: six one-pole sections plus a delayed tap.
const POLES: [(f32, f32); 6] = [
    (0.998_86, 0.055_518),
    (0.993_32, 0.075_076),
    (0.969, 0.153_852),
    (0.866_5, 0.310_486),
    (0.55, 0.532_952),
    (-0.761_6, -0.016_898),
];

/// Stationary fan: pink noise, blade-pass hum with harmonics, and rumble.
pub fn fan(seconds: f32, seed: u64, rms: f32) -> Vec<f32> {
    let total = (seconds * RATE as f32) as usize;
    let mut rng = Rng::new(seed);
    let mut pink = [0.0_f32; 6];
    let mut previous = 0.0_f32;
    let mut rumble = 0.0_f32;
    let mut output: Vec<f32> = (0..total)
        .map(|index| {
            let white = rng.signed();
            for (state, (pole, weight)) in pink.iter_mut().zip(POLES) {
                *state = pole * *state + weight * white;
            }
            let pinked = pink.iter().sum::<f32>() + previous + 0.536_2 * white;
            previous = 0.115_926 * white;
            rumble = 0.992f32.mul_add(rumble, 0.008 * rng.signed());
            let time = index as f32 / RATE as f32;
            let hum: f32 = [(1.0, 0.25), (2.0, 0.12), (3.0, 0.06)]
                .iter()
                .map(|(harmonic, amplitude)| amplitude * (TAU * 87.0 * harmonic * time).sin())
                .sum();
            0.1 * pinked + hum * 0.4 + 8.0 * rumble
        })
        .collect();
    let current = (output.iter().map(|x| x * x).sum::<f32>() / total as f32).sqrt();
    for sample in &mut output {
        *sample *= rms / current;
    }
    output
}

/// Loudspeaker-to-microphone path: a bulk delay, a direct tap, a dozen
/// decaying reflections over 60 ms, and a speaker low-pass.
pub fn room_echo(far: &[f32], delay: usize, gain: f32, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut taps = vec![(delay, 1.0_f32)];
    for _ in 0..12 {
        let offset = (rng.range(0.002, 0.06) * RATE as f32) as usize;
        let fade = (-(offset as f32) / (0.02 * RATE as f32)).exp();
        taps.push((delay + offset, 0.5 * fade * rng.signed()));
    }
    let mut speaker = 0.0_f32;
    let shaped: Vec<f32> = far
        .iter()
        .map(|sample| {
            speaker = 0.3f32.mul_add(speaker, 0.7 * sample);
            speaker
        })
        .collect();
    let mut echo = vec![0.0_f32; far.len()];
    for (offset, weight) in taps {
        for (out, sample) in echo.iter_mut().skip(offset).zip(&shaped) {
            *out += gain * weight * sample;
        }
    }
    echo
}

/// Sum of squares.
pub fn energy(samples: &[f32]) -> f64 {
    samples.iter().map(|x| f64::from(*x) * f64::from(*x)).sum()
}

/// Power ratio in dB.
pub fn db(numerator: f64, denominator: f64) -> f64 {
    10.0 * (numerator / denominator).log10()
}

/// Lag in `0..max_lag` that best aligns `output` with `input`.
pub fn best_lag(input: &[f32], output: &[f32], max_lag: usize) -> usize {
    let span = input.len().min(output.len()) - max_lag;
    let score = |lag: usize| -> f64 {
        input[..span]
            .iter()
            .zip(&output[lag..lag + span])
            .map(|(x, y)| f64::from(*x) * f64::from(*y))
            .sum()
    };
    let scores: Vec<f64> = (0..max_lag).map(score).collect();
    (0..max_lag)
        .max_by(|a, b| scores[*a].total_cmp(&scores[*b]))
        .unwrap_or(0)
}
