//! Per-IP limit for password login and registration.
//!
//! Five attempts may proceed immediately. The sixth waits until the minute
//! refills. Presenting an existing session token does not consume a slot.

use std::{net::IpAddr, num::NonZeroU32};

use governor::{Quota, RateLimiter, clock::DefaultClock, state::keyed::DefaultKeyedStateStore};

/// Five immediate attempts per address, then one replenished every 12 seconds.
const ATTEMPTS: NonZeroU32 = NonZeroU32::new(5).unwrap();

/// In-memory keyed limiter. One process, no persistence.
pub(crate) struct LoginLimiter {
    inner: RateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>,
}

impl LoginLimiter {
    pub(crate) fn new() -> Self {
        Self {
            inner: RateLimiter::keyed(Quota::per_minute(ATTEMPTS)),
        }
    }

    /// Records one attempt. `Err` means this address is over the quota.
    pub(crate) fn check(&self, peer: IpAddr) -> Result<(), ()> {
        self.inner.check_key(&peer).map_err(|_| ())
    }
}
