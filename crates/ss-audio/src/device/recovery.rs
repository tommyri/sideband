//! Which device a stream should be on, and when to try to get it there.
//!
//! A stream is opened on the selected device, or on the host default when
//! nothing is selected. When the device goes away the stream is dropped and
//! [`Recovery`] keeps asking for it: right away, on every device that appears
//! or default that changes, and every [`RETRY_INTERVAL`] in between. While a
//! selected device is missing the stream runs on the host default, and moves
//! back as soon as the selected device opens again.

use std::time::{Duration, Instant};

use super::watch::{DeviceEvent, DeviceId, Direction};

/// How long to wait before trying a failed open again. The inventory poll
/// runs at the same rate, so a device that returns is found within about a
/// second even if no event announced it.
pub const RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Where a direction's stream is, as the UI shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Open on the device that was asked for: the selected one, or the
    /// default when nothing is selected.
    Wanted,
    /// The selected device is missing; the stream runs on the host default.
    Fallback,
    /// No stream is open. Opening is retried.
    Missing,
}

/// One open to try.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// The device to open; `None` is the host default.
    pub device: Option<DeviceId>,
    /// When `device` fails, open the host default instead.
    pub fallback: bool,
}

/// Reopen bookkeeping for one direction. It holds no stream: the caller
/// opens and drops streams and reports the outcome.
#[derive(Debug, Clone)]
pub struct Recovery {
    direction: Direction,
    selected: Option<DeviceId>,
    open: Option<DeviceId>,
    /// The open stream follows a default that has since moved.
    stale: bool,
    /// When the next attempt is due. `None` while nothing needs doing.
    due: Option<Instant>,
}

impl Recovery {
    /// Nothing open yet; the first attempt is due at once. `selected` of
    /// `None` follows the host default.
    #[must_use]
    pub fn new(direction: Direction, selected: Option<DeviceId>, now: Instant) -> Self {
        Self {
            direction,
            selected,
            open: None,
            stale: false,
            due: Some(now),
        }
    }

    /// The direction this tracks.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// Where the stream is.
    #[must_use]
    pub fn route(&self) -> Route {
        match (&self.open, &self.selected) {
            (None, _) => Route::Missing,
            (Some(open), Some(selected)) if open != selected => Route::Fallback,
            (Some(_), _) => Route::Wanted,
        }
    }

    /// The open to try now, if one is due. With [`Route::Fallback`] it asks
    /// only for the selected device: the caller keeps the fallback stream
    /// when that fails.
    #[must_use]
    pub fn attempt(&self, now: Instant) -> Option<Attempt> {
        if self.due.is_none_or(|due| due > now) {
            return None;
        }
        let replace = self.open.is_none() || self.stale;
        match self.route() {
            Route::Wanted if !self.stale => None,
            Route::Fallback if !replace => Some(Attempt {
                device: self.selected.clone(),
                fallback: false,
            }),
            _ => Some(Attempt {
                device: self.selected.clone(),
                fallback: self.selected.is_some(),
            }),
        }
    }

    /// A stream opened on `id`. On the fallback, the selected device is
    /// tried again after [`RETRY_INTERVAL`].
    pub fn opened(&mut self, id: DeviceId, now: Instant) {
        self.open = Some(id);
        self.stale = false;
        self.due = (self.route() == Route::Fallback).then_some(now + RETRY_INTERVAL);
    }

    /// The attempt failed. Any stream that is still open stays.
    pub fn failed(&mut self, now: Instant) {
        self.due = Some(now + RETRY_INTERVAL);
    }

    /// The caller dropped the stream for another reason; reopen at once.
    pub fn closed(&mut self, now: Instant) {
        self.open = None;
        self.stale = false;
        self.wake(now);
    }

    /// Try again now if the stream is missing or on the fallback. Settings
    /// Apply uses this, so the user can force a retry.
    pub fn retry_now(&mut self, now: Instant) {
        if self.route() != Route::Wanted || self.stale {
            self.wake(now);
        }
    }

    /// Take a device event into account. Returns `true` when the open stream's
    /// device is lost: the caller drops that stream.
    pub fn observe(&mut self, event: &DeviceEvent, now: Instant) -> bool {
        match event {
            DeviceEvent::Lost { direction, id }
                if *direction == self.direction && self.open.as_ref() == Some(id) =>
            {
                self.closed(now);
                true
            }
            DeviceEvent::DefaultChanged { direction, id } if *direction == self.direction => {
                let follows = self.selected.is_none() || self.route() == Route::Fallback;
                match (&self.open, id) {
                    (None, _) => self.wake(now),
                    (Some(open), Some(new)) if follows && open != new => {
                        self.stale = true;
                        self.wake(now);
                    }
                    _ => {}
                }
                false
            }
            DeviceEvent::Added(info) => {
                let fits = match self.direction {
                    Direction::Capture => info.input,
                    Direction::Playback => info.output,
                };
                if fits && self.route() != Route::Wanted {
                    self.wake(now);
                }
                false
            }
            _ => false,
        }
    }

    fn wake(&mut self, now: Instant) {
        self.due = Some(self.due.map_or(now, |due| due.min(now)));
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Attempt, RETRY_INTERVAL, Recovery, Route};
    use crate::device::{DeviceEvent, DeviceId, DeviceInfo, Direction};

    fn id(raw: &str) -> DeviceId {
        DeviceId::new(raw)
    }

    fn added(raw: &str) -> DeviceEvent {
        DeviceEvent::Added(DeviceInfo {
            id: id(raw),
            name: raw.to_owned(),
            input: true,
            output: false,
        })
    }

    #[test]
    fn a_failed_open_is_retried_on_a_timer_and_on_device_events() {
        let start = Instant::now();
        let mut recovery = Recovery::new(Direction::Capture, Some(id("usb")), start);
        let first = recovery.attempt(start);
        assert_eq!(
            first,
            Some(Attempt {
                device: Some(id("usb")),
                fallback: true,
            })
        );
        recovery.failed(start);
        assert_eq!(recovery.route(), Route::Missing);
        assert_eq!(recovery.attempt(start + RETRY_INTERVAL / 2), None);
        assert_eq!(recovery.attempt(start + RETRY_INTERVAL), first);

        recovery.failed(start);
        assert!(!recovery.observe(&added("usb"), start + Duration::from_millis(10)));
        assert_eq!(recovery.attempt(start + Duration::from_millis(10)), first);
    }

    #[test]
    fn a_missing_selection_runs_on_the_default_and_returns() {
        let start = Instant::now();
        let mut recovery = Recovery::new(Direction::Capture, Some(id("usb")), start);
        recovery.opened(id("usb"), start);
        assert_eq!(recovery.route(), Route::Wanted);
        assert_eq!(recovery.attempt(start + RETRY_INTERVAL * 4), None);

        let lost = DeviceEvent::Lost {
            direction: Direction::Capture,
            id: id("usb"),
        };
        assert!(recovery.observe(&lost, start));
        recovery.opened(id("builtin"), start);
        assert_eq!(recovery.route(), Route::Fallback);
        // Only the selected device is asked for; the fallback stays open.
        let back = Some(Attempt {
            device: Some(id("usb")),
            fallback: false,
        });
        assert_eq!(recovery.attempt(start), None);
        assert_eq!(recovery.attempt(start + RETRY_INTERVAL), back);
        recovery.failed(start + RETRY_INTERVAL);
        assert_eq!(recovery.route(), Route::Fallback);
        let later = start + RETRY_INTERVAL + Duration::from_millis(1);
        recovery.observe(&added("usb"), later);
        assert_eq!(recovery.attempt(later), back);
        recovery.opened(id("usb"), later);
        assert_eq!(recovery.route(), Route::Wanted);
        assert_eq!(recovery.attempt(later + RETRY_INTERVAL * 4), None);
    }

    #[test]
    fn a_stream_on_the_default_follows_a_new_default() {
        let start = Instant::now();
        let mut recovery = Recovery::new(Direction::Playback, None, start);
        recovery.opened(id("speakers"), start);
        let other = DeviceEvent::DefaultChanged {
            direction: Direction::Capture,
            id: Some(id("airpods")),
        };
        assert!(!recovery.observe(&other, start));
        assert_eq!(recovery.attempt(start), None);
        let moved = DeviceEvent::DefaultChanged {
            direction: Direction::Playback,
            id: Some(id("airpods")),
        };
        assert!(!recovery.observe(&moved, start));
        assert_eq!(
            recovery.attempt(start),
            Some(Attempt {
                device: None,
                fallback: false,
            })
        );
        // The old stream keeps playing until the new one is open.
        recovery.failed(start);
        assert_eq!(recovery.route(), Route::Wanted);
        assert!(recovery.attempt(start + RETRY_INTERVAL).is_some());
        recovery.opened(id("airpods"), start + RETRY_INTERVAL);
        assert_eq!(recovery.attempt(start + RETRY_INTERVAL * 4), None);
    }

    #[test]
    fn a_selected_device_ignores_default_changes_and_apply_only_wakes_a_degraded_stream() {
        let start = Instant::now();
        let mut recovery = Recovery::new(Direction::Capture, Some(id("usb")), start);
        recovery.opened(id("usb"), start);
        let moved = DeviceEvent::DefaultChanged {
            direction: Direction::Capture,
            id: Some(id("airpods")),
        };
        recovery.observe(&moved, start);
        recovery.observe(&added("airpods"), start);
        recovery.retry_now(start);
        assert_eq!(recovery.attempt(start), None);

        recovery.closed(start);
        recovery.failed(start);
        assert_eq!(recovery.attempt(start), None);
        recovery.retry_now(start);
        assert!(recovery.attempt(start).is_some());
    }
}
