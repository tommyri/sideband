//! Rate limits shared by the control stream and the public HTTP listener.
//!
//! Every quota is an in-memory GCRA bucket (`governor`) that lives for the
//! process. A rejection carries how long the caller must wait; the control
//! stream sends it as `Error.retry_after_ms` and HTTP as `Retry-After`.
//! `docs/security.md` has the full table, including the per-feature limits
//! that live next to their handlers.

use std::{
    collections::HashMap,
    hash::Hash,
    net::IpAddr,
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use governor::{
    Quota, RateLimiter,
    clock::{Clock, DefaultClock},
    state::keyed::DefaultKeyedStateStore,
};
use parking_lot::Mutex;

/// Every `Hello` from one address, whatever the credential: a burst of 30,
/// then one every two seconds. Password attempts also pay the stricter
/// login quota in `auth`.
pub const HELLO_BURST: u32 = 30;
/// Refill interval for [`HELLO_BURST`].
pub const HELLO_PERIOD: Duration = Duration::from_secs(2);
/// Control commands per account, across its connections: a burst of 100,
/// then twenty a second. `Ping` is exempt so keepalive never fails.
pub const COMMAND_BURST: u32 = 100;
/// Refill interval for [`COMMAND_BURST`].
pub const COMMAND_PERIOD: Duration = Duration::from_millis(50);
/// Concurrent QUIC sessions, pending handshakes included, from one address.
pub const SESSIONS_PER_IP: usize = 32;
/// Public HTTP requests from one address: a burst of 200, then fifty a second.
pub const HTTP_BURST: u32 = 200;
/// Refill interval for [`HTTP_BURST`].
pub const HTTP_PERIOD: Duration = Duration::from_millis(20);
/// Uploads per account: a burst of ten, then one every six seconds. The
/// rolling daily byte quota in `storage` still applies.
pub const UPLOAD_BURST: u32 = 10;
/// Refill interval for [`UPLOAD_BURST`].
pub const UPLOAD_PERIOD: Duration = Duration::from_secs(6);

/// Keys are pruned once this many checks have passed since the last prune.
const PRUNE_EVERY: u64 = 4096;

/// The address a per-address quota is keyed by.
///
/// An IPv6 host usually controls a whole /64, so IPv6 addresses are reduced
/// to their /64 prefix; otherwise one machine could rotate through 2^64
/// "addresses". IPv4-mapped IPv6 addresses count as the IPv4 address.
#[must_use]
pub fn peer_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or_else(
            || {
                let [a, b, c, d, ..] = v6.segments();
                IpAddr::V6(std::net::Ipv6Addr::new(a, b, c, d, 0, 0, 0, 0))
            },
            IpAddr::V4,
        ),
    }
}

/// A quota of `burst` immediate cells that refill one per `period`.
///
/// # Panics
///
/// Never for the constants in this module: `burst` is checked to be nonzero
/// and `period` is nonzero.
#[must_use]
pub fn quota(burst: u32, period: Duration) -> Quota {
    let burst = NonZeroU32::new(burst.max(1)).unwrap_or(NonZeroU32::MIN);
    // `with_period` is `None` only for a zero period.
    Quota::with_period(period.max(Duration::from_nanos(1)))
        .unwrap_or_else(|| Quota::per_second(NonZeroU32::MAX))
        .allow_burst(burst)
}

/// One keyed bucket per `K`. Idle keys are dropped as the map grows.
pub struct KeyedLimiter<K: Hash + Eq + Clone> {
    inner: RateLimiter<K, DefaultKeyedStateStore<K>, DefaultClock>,
    checks: AtomicU64,
}

impl<K: Hash + Eq + Clone> KeyedLimiter<K> {
    /// A limiter with `quota` for every key.
    #[must_use]
    pub fn new(quota: Quota) -> Self {
        Self {
            inner: RateLimiter::keyed(quota),
            checks: AtomicU64::new(0),
        }
    }

    /// Spends one cell for `key`. `Err` is how long until one is available.
    pub fn check(&self, key: &K) -> Result<(), Duration> {
        if self.checks.fetch_add(1, Ordering::Relaxed) % PRUNE_EVERY == PRUNE_EVERY - 1 {
            self.inner.retain_recent();
            self.inner.shrink_to_fit();
        }
        self.inner
            .check_key(key)
            .map_err(|denied| denied.wait_time_from(DefaultClock::default().now()))
    }
}

/// Counts live sessions per address and refuses past a cap.
pub struct PeerSessions {
    max: AtomicUsize,
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl Default for PeerSessions {
    fn default() -> Self {
        Self {
            max: AtomicUsize::new(SESSIONS_PER_IP),
            counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl PeerSessions {
    /// Changes the cap for later sessions. Live sessions are not closed.
    pub fn set_max(&self, max: usize) {
        self.max.store(max, Ordering::Relaxed);
    }

    /// A slot for `peer`, released on drop. `None` when `peer` is at the cap.
    #[must_use]
    pub fn acquire(&self, peer: IpAddr) -> Option<PeerSlot> {
        let peer = peer_key(peer);
        let max = self.max.load(Ordering::Relaxed);
        let mut counts = self.counts.lock();
        let count = counts.entry(peer).or_insert(0);
        if *count >= max {
            return None;
        }
        *count += 1;
        Some(PeerSlot {
            peer,
            counts: Arc::clone(&self.counts),
        })
    }
}

/// One counted session. Dropping it frees the slot.
pub struct PeerSlot {
    peer: IpAddr,
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut counts = self.counts.lock();
        if let Some(count) = counts.get_mut(&self.peer) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.peer);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{net::IpAddr, time::Duration};

    use super::{KeyedLimiter, PeerSessions, peer_key, quota};

    #[test]
    fn ipv6_peers_are_keyed_by_their_slash_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::2".parse().unwrap();
        let other: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(peer_key(a), peer_key(b));
        assert_ne!(peer_key(a), peer_key(other));
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().unwrap();
        assert_eq!(peer_key(mapped), "192.0.2.7".parse::<IpAddr>().unwrap());
        let v4: IpAddr = "192.0.2.7".parse().unwrap();
        assert_eq!(peer_key(v4), v4);
    }

    #[test]
    fn a_rejection_says_how_long_to_wait() {
        let limiter = KeyedLimiter::new(quota(2, Duration::from_secs(10)));
        assert!(limiter.check(&1).is_ok());
        assert!(limiter.check(&1).is_ok());
        let wait = limiter.check(&1).unwrap_err();
        assert!(
            wait > Duration::from_secs(9) && wait <= Duration::from_secs(10),
            "{wait:?}"
        );
        assert!(limiter.check(&2).is_ok(), "keys are independent");
    }

    #[test]
    fn pruning_keeps_busy_keys_limited() {
        let limiter = KeyedLimiter::new(quota(1, Duration::from_secs(60)));
        assert!(limiter.check(&0_u64).is_ok());
        for key in 1..5000_u64 {
            let _ = limiter.check(&key);
        }
        assert!(limiter.check(&0).is_err());
    }

    #[test]
    fn peer_slots_are_counted_per_address_and_released_on_drop() {
        let sessions = PeerSessions::default();
        sessions.set_max(2);
        let local: IpAddr = "127.0.0.1".parse().unwrap();
        let other: IpAddr = "192.0.2.1".parse().unwrap();
        let first = sessions.acquire(local).unwrap();
        let _second = sessions.acquire(local).unwrap();
        assert!(sessions.acquire(local).is_none());
        assert!(sessions.acquire(other).is_some());
        drop(first);
        assert!(sessions.acquire(local).is_some());
    }
}
