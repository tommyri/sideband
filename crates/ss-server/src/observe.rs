//! Process counters rendered by the internal listener.
//!
//! Gauges are sampled on each scrape. Message inserts, sessions, slow-consumer
//! disconnects, and write-lock hold time update as the work happens. None of
//! these are on the datagram path.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static MESSAGES: AtomicU64 = AtomicU64::new(0);
static SESSIONS: AtomicU64 = AtomicU64::new(0);
static SLOW_DISCONNECTS: AtomicU64 = AtomicU64::new(0);

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

/// One `Hello` accepted on the authenticated listener.
pub fn record_session() {
    SESSIONS.fetch_add(1, Ordering::Relaxed);
}

/// Authenticated sessions opened since process start.
#[must_use]
pub fn sessions() -> u64 {
    SESSIONS.load(Ordering::Relaxed)
}

/// The server closed a connection because its control consumer fell behind.
pub fn record_slow_disconnect() {
    SLOW_DISCONNECTS.fetch_add(1, Ordering::Relaxed);
}

/// Slow-consumer disconnects since process start.
#[must_use]
pub fn slow_disconnects() -> u64 {
    SLOW_DISCONNECTS.load(Ordering::Relaxed)
}
