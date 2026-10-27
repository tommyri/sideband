//! Process counters rendered by the internal listener.
//!
//! Gauges are sampled on each scrape. Message inserts and write-lock hold
//! time update the counter and histogram as the work happens.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static MESSAGES: AtomicU64 = AtomicU64::new(0);

/// One stored message, from a user or a webhook.
pub fn record_message() {
    MESSAGES.fetch_add(1, Ordering::Relaxed);
}

/// How long a writer held the database lock.
pub fn record_db(elapsed: Duration) {
    metrics::histogram!("socialspeak_db_seconds").record(elapsed.as_secs_f64());
}

/// Messages inserted since process start.
#[must_use]
pub fn messages() -> u64 {
    MESSAGES.load(Ordering::Relaxed)
}
