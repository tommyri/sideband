//! Per-stage latency and CPU counters, shared through atomics.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use super::{Backend, CHAIN_BUDGET, Stage};

/// CPU time spent per 10 ms frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuStats {
    /// Frames measured since the chain was built.
    pub frames: u64,
    /// The most recent frame.
    pub last: Duration,
    /// Mean over every measured frame.
    pub mean: Duration,
    /// Slowest frame, including the first, which may warm caches.
    pub max: Duration,
}

/// What the settings UI shows for one stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageReport {
    /// Position in the chain.
    pub stage: Stage,
    /// Library behind it.
    pub backend: Backend,
    /// Algorithmic delay the stage adds.
    pub latency: Duration,
    /// Measured processing time.
    pub cpu: CpuStats,
}

#[derive(Default)]
struct Counter {
    frames: AtomicU64,
    last_ns: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
}

impl Counter {
    fn record(&self, elapsed: Duration) {
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        self.last_ns.store(nanos, Ordering::Relaxed);
        self.total_ns.fetch_add(nanos, Ordering::Relaxed);
        self.max_ns.fetch_max(nanos, Ordering::Relaxed);
        self.frames.fetch_add(1, Ordering::Relaxed);
    }

    fn load(&self) -> CpuStats {
        let frames = self.frames.load(Ordering::Relaxed);
        let total = self.total_ns.load(Ordering::Relaxed);
        CpuStats {
            frames,
            last: Duration::from_nanos(self.last_ns.load(Ordering::Relaxed)),
            mean: Duration::from_nanos(total.checked_div(frames).unwrap_or(0)),
            max: Duration::from_nanos(self.max_ns.load(Ordering::Relaxed)),
        }
    }
}

struct Slot {
    stage: Stage,
    backend: Backend,
    latency: Duration,
    cpu: Counter,
}

struct Shared {
    slots: Box<[Slot]>,
    chain: Counter,
    over_budget: AtomicU64,
}

/// Read-only view of a chain's meters. Cloning shares the same counters;
/// reading never blocks the capture worker.
#[derive(Clone)]
pub struct Monitor {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Monitor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.stages()).finish()
    }
}

impl Monitor {
    /// One report per enabled stage, in chain order.
    pub fn stages(&self) -> impl ExactSizeIterator<Item = StageReport> + '_ {
        self.shared.slots.iter().map(|slot| StageReport {
            stage: slot.stage,
            backend: slot.backend,
            latency: slot.latency,
            cpu: slot.cpu.load(),
        })
    }

    /// Whole-chain processing time per frame.
    #[must_use]
    pub fn chain(&self) -> CpuStats {
        self.shared.chain.load()
    }

    /// Frames whose whole-chain time exceeded [`CHAIN_BUDGET`].
    #[must_use]
    pub fn over_budget(&self) -> u64 {
        self.shared.over_budget.load(Ordering::Relaxed)
    }

    /// Sum of the enabled stages' added latency.
    #[must_use]
    pub fn latency(&self) -> Duration {
        self.shared.slots.iter().map(|slot| slot.latency).sum()
    }
}

pub(super) struct Meter {
    shared: Arc<Shared>,
}

impl Meter {
    pub(super) fn new(stages: impl Iterator<Item = (Stage, Backend, Duration)>) -> Self {
        let slots = stages
            .map(|(stage, backend, latency)| Slot {
                stage,
                backend,
                latency,
                cpu: Counter::default(),
            })
            .collect();
        Self {
            shared: Arc::new(Shared {
                slots,
                chain: Counter::default(),
                over_budget: AtomicU64::new(0),
            }),
        }
    }

    pub(super) fn monitor(&self) -> Monitor {
        Monitor {
            shared: Arc::clone(&self.shared),
        }
    }

    pub(super) fn start(&self) -> Timer<'_> {
        let now = Instant::now();
        Timer {
            shared: &self.shared,
            start: now,
            mark: now,
        }
    }
}

pub(super) struct Timer<'meter> {
    shared: &'meter Shared,
    start: Instant,
    mark: Instant,
}

impl Timer<'_> {
    pub(super) fn lap(&mut self, index: usize) {
        let now = Instant::now();
        if let Some(slot) = self.shared.slots.get(index) {
            slot.cpu.record(now - self.mark);
        }
        self.mark = now;
    }

    pub(super) fn finish(self) {
        let total = self.mark - self.start;
        self.shared.chain.record(total);
        if total > CHAIN_BUDGET {
            self.shared.over_budget.fetch_add(1, Ordering::Relaxed);
        }
    }
}
