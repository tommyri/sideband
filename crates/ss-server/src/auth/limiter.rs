//! Password login and registration quotas.
//!
//! Per address (the hard gate): five attempts may proceed immediately, then
//! one every 12 seconds. Every password login and registration pays it.
//!
//! Per username, only *failed* logins count: ten failures, then one more
//! allowed every six minutes. Once a username is over that budget it is
//! "locked", but only for addresses that have themselves failed against it
//! in the last [`TAINT_FOR`]. A correct password from any other address still
//! gets through, so nobody can lock an account out by guessing at it. A
//! botnet spread over many addresses gets one guess per address per
//! [`TAINT_FOR`] once the lock is on, on top of the per-address gate.
//!
//! Presenting an existing session token consumes none of this.

use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use crate::limits::{KeyedLimiter, peer_key, quota};

/// Immediate attempts per address.
pub(crate) const IP_BURST: u32 = 5;
/// One more attempt per address after this long.
pub(crate) const IP_PERIOD: Duration = Duration::from_secs(12);
/// Failed logins per username before it locks.
pub(crate) const USER_BURST: u32 = 10;
/// One more failure allowed per username after this long.
pub(crate) const USER_PERIOD: Duration = Duration::from_mins(6);
/// How long an address that failed against a username stays held back from
/// it while the username is locked.
pub(crate) const TAINT_FOR: Duration = Duration::from_mins(15);

/// Entries are swept once a map holds this many.
const SWEEP_AT: usize = 4096;

type Pair = (String, IpAddr);

/// In-memory login quotas. One process, no persistence.
pub(crate) struct LoginLimiter {
    by_ip: KeyedLimiter<IpAddr>,
    failures: KeyedLimiter<String>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Username → when its failure budget refills enough to unlock.
    locked: HashMap<String, Instant>,
    /// (username, address prefix) → when the last failure stops counting.
    tainted: HashMap<Pair, Instant>,
}

impl LoginLimiter {
    pub(crate) fn new() -> Self {
        Self {
            by_ip: KeyedLimiter::new(quota(IP_BURST, IP_PERIOD)),
            failures: KeyedLimiter::new(quota(USER_BURST, USER_PERIOD)),
            state: Mutex::new(State::default()),
        }
    }

    /// Records one attempt from `peer`. `Err` is how long until the next one.
    pub(crate) fn check(&self, peer: IpAddr) -> Result<(), Duration> {
        self.by_ip.check(&peer_key(peer))
    }

    /// Whether `peer` may try a password for `username` now. Spends nothing.
    /// `Err` only when the username is locked *and* `peer` has failed
    /// against it recently.
    pub(crate) fn admit(&self, username: &str, peer: IpAddr) -> Result<(), Duration> {
        let now = Instant::now();
        let name = username.to_ascii_lowercase();
        let state = self.state.lock();
        let Some(&unlock) = state.locked.get(&name).filter(|until| **until > now) else {
            return Ok(());
        };
        let Some(&clean) = state
            .tainted
            .get(&(name, peer_key(peer)))
            .filter(|until| **until > now)
        else {
            return Ok(());
        };
        Err(unlock.min(clean).saturating_duration_since(now))
    }

    /// A wrong password, or an unknown username, from `peer`.
    pub(crate) fn failed(&self, username: &str, peer: IpAddr) {
        let now = Instant::now();
        let name = username.to_ascii_lowercase();
        let over = self.failures.check(&name).err();
        let mut state = self.state.lock();
        if state.tainted.len() >= SWEEP_AT || state.locked.len() >= SWEEP_AT {
            state.tainted.retain(|_, until| *until > now);
            state.locked.retain(|_, until| *until > now);
        }
        state
            .tainted
            .insert((name.clone(), peer_key(peer)), now + TAINT_FOR);
        if let Some(wait) = over {
            state.locked.insert(name, now + wait);
        }
    }

    /// A correct password from `peer`: that address is trusted for the
    /// username again. The username's failure budget is left as it is.
    pub(crate) fn succeeded(&self, username: &str, peer: IpAddr) {
        let name = username.to_ascii_lowercase();
        self.state.lock().tainted.remove(&(name, peer_key(peer)));
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::{LoginLimiter, USER_BURST};

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([198, 51, 100, last])
    }

    #[test]
    fn a_locked_username_still_admits_addresses_that_never_failed() {
        let limiter = LoginLimiter::new();
        for attempt in 0..=USER_BURST {
            assert!(limiter.admit("Victim", ip(1)).is_ok(), "attempt {attempt}");
            limiter.failed("Victim", ip(1));
        }
        let wait = limiter.admit("victim", ip(1)).unwrap_err();
        assert!(wait.as_secs() > 60, "{wait:?}");
        assert!(
            limiter.admit("victim", ip(2)).is_ok(),
            "the owner's address"
        );
        assert!(limiter.admit("bystander", ip(1)).is_ok(), "other accounts");
        limiter.succeeded("victim", ip(1));
        assert!(
            limiter.admit("victim", ip(1)).is_ok(),
            "a success clears it"
        );
    }

    #[test]
    fn failures_below_the_budget_never_hold_anyone_back() {
        let limiter = LoginLimiter::new();
        for _ in 0..USER_BURST {
            limiter.failed("ada", ip(3));
        }
        assert!(limiter.admit("ada", ip(3)).is_ok());
    }
}
