//! Press/release state for one push-to-talk chord.
//!
//! Operating-system backends feed [`InputEvent`]s in and poll
//! [`PttStateMachine::release_deadline`]. They do not implement debounce or
//! the release delay themselves.

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use crate::binding::{Binding, Code, InputEvent, MouseButton};

/// Gaps between a release and the next press that are coalesced.
///
/// A physical release followed by a press at or before this duration does
/// not emit [`PttEvent::Released`]. The effective wait before a release is
/// `max(release_delay, DEBOUNCE)`, so a configured delay of zero still
/// swallows keyboard chatter.
pub const DEBOUNCE: Duration = Duration::from_millis(30);

/// Release delay used when the caller does not pass one.
///
/// Matches the voice-session default: keep transmit open briefly after the
/// key comes up so the end of a word is not clipped.
pub const DEFAULT_RELEASE_DELAY: Duration = Duration::from_millis(150);

/// Transmit edge emitted by [`PttStateMachine`].
///
/// `Pressed` opens the gate. `Released` closes it. The machine emits each
/// edge once per transition, including through the debounce and release-delay
/// window, where it stays logically pressed without a new event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PttEvent {
    /// The chord just became held. Start transmitting.
    Pressed,
    /// The chord has been released and the delay has elapsed. Stop transmitting.
    Released,
}

impl std::fmt::Display for PttEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Pressed => "pressed",
            Self::Released => "released",
        })
    }
}

/// Physical-to-logical push-to-talk state.
///
/// Feed edges with [`Self::handle`] and the clock with [`Self::poll`].
/// [`Self::release_deadline`] is when the next poll may emit
/// [`PttEvent::Released`]. [`Self::force_release`] drops a stuck key after
/// focus loss, sleep, or a disabled event tap.
///
/// ```
/// use std::time::{Duration, Instant};
///
/// use ss_input::{Binding, Code, InputEvent, PttEvent, PttStateMachine};
///
/// let mut ptt = PttStateMachine::with_release_delay(
///     [Binding::key(Code::KeyV)],
///     Duration::ZERO,
/// );
/// let now = Instant::now();
/// assert_eq!(
///     ptt.handle(InputEvent::key_down(Code::KeyV), now),
///     vec![PttEvent::Pressed],
/// );
/// assert!(ptt.is_transmitting());
/// ```
#[derive(Debug, Clone)]
pub struct PttStateMachine {
    bindings: Vec<Binding>,
    release_delay: Duration,
    keys: HashSet<Code>,
    buttons: HashSet<MouseButton>,
    /// Downs ignored until the matching up. Filled by [`Self::force_release`].
    suppressed_keys: HashSet<Code>,
    suppressed_buttons: HashSet<MouseButton>,
    phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Held,
    PendingRelease {
        released_at: Instant,
        until: Instant,
    },
}

impl PttStateMachine {
    /// Machine for `bindings` with [`DEFAULT_RELEASE_DELAY`].
    #[must_use]
    pub fn new(bindings: impl Into<Vec<Binding>>) -> Self {
        Self::with_release_delay(bindings, DEFAULT_RELEASE_DELAY)
    }

    /// Machine for `bindings` that waits `release_delay` after a physical release.
    ///
    /// The wait is at least [`DEBOUNCE`], even when `release_delay` is shorter.
    #[must_use]
    pub fn with_release_delay(bindings: impl Into<Vec<Binding>>, release_delay: Duration) -> Self {
        Self {
            bindings: bindings.into(),
            release_delay,
            keys: HashSet::new(),
            buttons: HashSet::new(),
            suppressed_keys: HashSet::new(),
            suppressed_buttons: HashSet::new(),
            phase: Phase::Idle,
        }
    }

    /// Bindings currently watched. Any one of them can open transmit.
    #[must_use]
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Configured release delay, before the debounce floor is applied.
    #[must_use]
    pub const fn release_delay(&self) -> Duration {
        self.release_delay
    }

    /// `true` from [`PttEvent::Pressed`] until [`PttEvent::Released`].
    ///
    /// This stays true during the debounce and release-delay window.
    #[must_use]
    pub const fn is_transmitting(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// When [`Self::poll`] will emit [`PttEvent::Released`], if a release is pending.
    ///
    /// Backends sleep until this instant instead of spinning. `None` means
    /// the machine is idle or the chord is still held.
    #[must_use]
    pub const fn release_deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::PendingRelease { until, .. } => Some(until),
            Phase::Idle | Phase::Held => None,
        }
    }

    /// Apply one key or button edge at `now`.
    ///
    /// Returns zero, one, or two events. Two events happen only when a press
    /// arrives after a pending release has already come due and nobody polled:
    /// [`PttEvent::Released`] then [`PttEvent::Pressed`].
    #[must_use]
    pub fn handle(&mut self, event: InputEvent, now: Instant) -> Vec<PttEvent> {
        self.apply(event);
        self.reconcile(now)
    }

    /// Emit a delayed release if `now` has reached [`Self::release_deadline`].
    #[must_use]
    pub fn poll(&mut self, now: Instant) -> Option<PttEvent> {
        match self.phase {
            Phase::PendingRelease { until, .. } if !self.chord_down() && now >= until => {
                self.phase = Phase::Idle;
                Some(PttEvent::Released)
            }
            Phase::Idle | Phase::Held | Phase::PendingRelease { .. } => None,
        }
    }

    /// Replace the watched bindings and emit any edge that change causes.
    #[must_use]
    pub fn update_bindings(
        &mut self,
        bindings: impl Into<Vec<Binding>>,
        now: Instant,
    ) -> Vec<PttEvent> {
        self.bindings = bindings.into();
        self.reconcile(now)
    }

    /// Change the release delay and emit a release if the new deadline has passed.
    ///
    /// A pending release keeps its original start time. Shortening the delay
    /// can make [`Self::poll`] due immediately; this method emits that release
    /// when `now` is already past the new deadline.
    #[must_use]
    pub fn set_release_delay(&mut self, delay: Duration, now: Instant) -> Vec<PttEvent> {
        self.release_delay = delay;
        if let Phase::PendingRelease { released_at, .. } = self.phase {
            self.phase = Phase::PendingRelease {
                released_at,
                until: deadline_after(released_at, self.release_wait()),
            };
        }
        self.reconcile(now)
    }

    /// Clear tracked keys and emit [`PttEvent::Released`] if transmit was open.
    ///
    /// Focus loss, sleep, and a disabled event tap use this so a missed key-up
    /// cannot leave the gate open. Keys and buttons that were down are
    /// suppressed until the matching up arrives, so an auto-repeat down does
    /// not press again. A backend that sees the physical key is already up
    /// must inject that up; the up lifts the suppression.
    #[must_use]
    pub fn force_release(&mut self) -> Option<PttEvent> {
        self.suppressed_keys.extend(self.keys.drain());
        self.suppressed_buttons.extend(self.buttons.drain());
        match self.phase {
            Phase::Idle => None,
            Phase::Held | Phase::PendingRelease { .. } => {
                self.phase = Phase::Idle;
                Some(PttEvent::Released)
            }
        }
    }

    fn apply(&mut self, event: InputEvent) {
        match event {
            InputEvent::Key {
                code,
                pressed: true,
            } => {
                if self.suppressed_keys.contains(&code) {
                    return;
                }
                self.keys.insert(code);
            }
            InputEvent::Key {
                code,
                pressed: false,
            } => {
                self.suppressed_keys.remove(&code);
                self.keys.remove(&code);
            }
            InputEvent::Mouse {
                button,
                pressed: true,
            } => {
                if self.suppressed_buttons.contains(&button) {
                    return;
                }
                self.buttons.insert(button);
            }
            InputEvent::Mouse {
                button,
                pressed: false,
            } => {
                self.suppressed_buttons.remove(&button);
                self.buttons.remove(&button);
            }
        }
    }

    fn reconcile(&mut self, now: Instant) -> Vec<PttEvent> {
        let held = self.chord_down();
        match (self.phase, held) {
            (Phase::Idle, true) => {
                self.phase = Phase::Held;
                vec![PttEvent::Pressed]
            }
            (Phase::Held, false) => {
                let until = deadline_after(now, self.release_wait());
                self.phase = Phase::PendingRelease {
                    released_at: now,
                    until,
                };
                Vec::new()
            }
            (Phase::PendingRelease { until, .. }, true) if now <= until => {
                self.phase = Phase::Held;
                Vec::new()
            }
            (Phase::PendingRelease { .. }, true) => {
                self.phase = Phase::Held;
                vec![PttEvent::Released, PttEvent::Pressed]
            }
            (Phase::PendingRelease { until, .. }, false) if now >= until => {
                self.phase = Phase::Idle;
                vec![PttEvent::Released]
            }
            (Phase::Idle | Phase::PendingRelease { .. }, false) | (Phase::Held, true) => Vec::new(),
        }
    }

    fn chord_down(&self) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.is_held(&self.keys, &self.buttons))
    }

    fn release_wait(&self) -> Duration {
        self.release_delay.max(DEBOUNCE)
    }
}

fn deadline_after(from: Instant, wait: Duration) -> Instant {
    from.checked_add(wait).unwrap_or(from)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::{
        binding::{Binding, Code, InputEvent, ModifierSet, MouseButton},
        state::{DEBOUNCE, DEFAULT_RELEASE_DELAY, PttEvent, PttStateMachine},
    };

    fn machine(binding: Binding, release_delay: Duration) -> PttStateMachine {
        PttStateMachine::with_release_delay([binding], release_delay)
    }

    fn quiet(events: &[PttEvent]) {
        assert_eq!(events, []);
    }

    fn earlier(at: Instant, by: Duration) -> Instant {
        at.checked_sub(by).expect("test clock stays in range")
    }

    #[test]
    fn new_uses_the_voice_session_release_delay() {
        let ptt = PttStateMachine::new([Binding::key(Code::KeyV)]);
        assert_eq!(ptt.release_delay(), DEFAULT_RELEASE_DELAY);
        assert_eq!(DEFAULT_RELEASE_DELAY, Duration::from_millis(150));
    }

    #[test]
    fn debounce_coalesces_a_release_and_press_inside_30ms() {
        let mut ptt = machine(Binding::key(Code::KeyV), Duration::ZERO);
        let t0 = Instant::now();
        assert_eq!(
            ptt.handle(InputEvent::key_down(Code::KeyV), t0),
            vec![PttEvent::Pressed],
        );

        let released = t0 + Duration::from_millis(100);
        quiet(&ptt.handle(InputEvent::key_up(Code::KeyV), released));
        assert!(ptt.is_transmitting());
        assert_eq!(ptt.release_deadline(), Some(released + DEBOUNCE));

        let repress = released + Duration::from_millis(20);
        quiet(&ptt.handle(InputEvent::key_down(Code::KeyV), repress));
        assert!(ptt.is_transmitting());
        assert!(ptt.release_deadline().is_none());

        let mut boundary = machine(Binding::key(Code::KeyV), Duration::ZERO);
        assert_eq!(
            boundary.handle(InputEvent::key_down(Code::KeyV), t0),
            vec![PttEvent::Pressed],
        );
        quiet(&boundary.handle(InputEvent::key_up(Code::KeyV), t0));
        quiet(&boundary.handle(InputEvent::key_down(Code::KeyV), t0 + DEBOUNCE));
        assert!(boundary.is_transmitting());
    }

    #[test]
    fn debounce_is_a_floor_under_a_shorter_release_delay() {
        let mut ptt = machine(Binding::key(Code::Space), Duration::from_millis(10));
        let t0 = Instant::now();
        assert_eq!(
            ptt.handle(InputEvent::key_down(Code::Space), t0),
            vec![PttEvent::Pressed],
        );
        quiet(&ptt.handle(InputEvent::key_up(Code::Space), t0));
        assert_eq!(ptt.release_deadline(), Some(t0 + DEBOUNCE));
        assert!(ptt.poll(t0 + Duration::from_millis(10)).is_none());
        assert_eq!(ptt.poll(t0 + DEBOUNCE), Some(PttEvent::Released));
        assert!(!ptt.is_transmitting());
    }

    #[test]
    fn press_after_the_debounce_window_is_a_new_edge() {
        let mut ptt = machine(Binding::key(Code::KeyV), Duration::ZERO);
        let t0 = Instant::now();
        assert_eq!(
            ptt.handle(InputEvent::key_down(Code::KeyV), t0),
            vec![PttEvent::Pressed],
        );
        quiet(&ptt.handle(InputEvent::key_up(Code::KeyV), t0));
        assert_eq!(ptt.poll(t0 + DEBOUNCE), Some(PttEvent::Released));
        assert!(ptt.poll(t0 + DEBOUNCE).is_none());

        let mut late = machine(Binding::key(Code::KeyV), Duration::ZERO);
        assert_eq!(
            late.handle(InputEvent::key_down(Code::KeyV), t0),
            vec![PttEvent::Pressed],
        );
        quiet(&late.handle(InputEvent::key_up(Code::KeyV), t0));
        assert_eq!(
            late.handle(
                InputEvent::key_down(Code::KeyV),
                t0 + DEBOUNCE + Duration::from_nanos(1),
            ),
            vec![PttEvent::Released, PttEvent::Pressed],
        );
    }

    #[test]
    fn release_delay_holds_transmit_open() {
        let delay = Duration::from_millis(100);
        let mut ptt = machine(Binding::key(Code::KeyV), delay);
        let t0 = Instant::now();
        assert_eq!(
            ptt.handle(InputEvent::key_down(Code::KeyV), t0),
            vec![PttEvent::Pressed],
        );
        let up_at = t0 + Duration::from_millis(10);
        quiet(&ptt.handle(InputEvent::key_up(Code::KeyV), up_at));
        assert!(ptt.is_transmitting());
        assert!(
            ptt.poll(earlier(up_at + delay, Duration::from_millis(1)))
                .is_none()
        );
        assert_eq!(ptt.poll(up_at + delay), Some(PttEvent::Released));
        assert!(!ptt.is_transmitting());
    }

    #[test]
    fn repress_during_the_release_delay_cancels_the_release() {
        let delay = Duration::from_millis(100);
        let mut ptt = machine(Binding::mouse(MouseButton::Mouse4), delay);
        let t0 = Instant::now();
        assert_eq!(
            ptt.handle(InputEvent::mouse_down(MouseButton::Mouse4), t0),
            vec![PttEvent::Pressed],
        );
        quiet(&ptt.handle(InputEvent::mouse_up(MouseButton::Mouse4), t0));
        let repress = t0 + Duration::from_millis(50);
        quiet(&ptt.handle(InputEvent::mouse_down(MouseButton::Mouse4), repress));
        assert!(ptt.is_transmitting());
        assert!(ptt.release_deadline().is_none());
        assert!(ptt.poll(t0 + delay).is_none());

        let second_up = repress + Duration::from_millis(5);
        quiet(&ptt.handle(InputEvent::mouse_up(MouseButton::Mouse4), second_up));
        assert!(
            ptt.poll(earlier(second_up + delay, Duration::from_nanos(1)))
                .is_none()
        );
        assert_eq!(ptt.poll(second_up + delay), Some(PttEvent::Released));
    }

    #[test]
    fn force_release_recovers_from_a_stuck_key() {
        let mut ptt = machine(Binding::key(Code::KeyV), Duration::from_millis(500));
        let t0 = Instant::now();
        assert_eq!(
            ptt.handle(InputEvent::key_down(Code::KeyV), t0),
            vec![PttEvent::Pressed],
        );
        assert_eq!(ptt.force_release(), Some(PttEvent::Released));
        assert!(!ptt.is_transmitting());
        assert!(ptt.release_deadline().is_none());

        quiet(&ptt.handle(InputEvent::key_down(Code::KeyV), t0));
        assert!(!ptt.is_transmitting());
        assert!(ptt.poll(t0 + Duration::from_secs(1)).is_none());

        quiet(&ptt.handle(InputEvent::key_up(Code::KeyV), t0));
        assert_eq!(
            ptt.handle(InputEvent::key_down(Code::KeyV), t0),
            vec![PttEvent::Pressed],
        );
        assert_eq!(ptt.force_release(), Some(PttEvent::Released));
        assert_eq!(ptt.force_release(), None);

        let mut pending = machine(Binding::key(Code::KeyV), Duration::from_millis(200));
        assert_eq!(
            pending.handle(InputEvent::key_down(Code::KeyV), t0),
            vec![PttEvent::Pressed],
        );
        quiet(&pending.handle(InputEvent::key_up(Code::KeyV), t0));
        assert!(pending.is_transmitting());
        assert_eq!(pending.force_release(), Some(PttEvent::Released));
        assert!(pending.poll(t0 + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn modifier_combo_requires_every_modifier_and_accepts_either_side() {
        let binding = Binding::key(Code::KeyV).with_modifiers(ModifierSet::CTRL.with_shift());
        let mut ptt = machine(binding, Duration::ZERO);
        let t0 = Instant::now();

        quiet(&ptt.handle(InputEvent::key_down(Code::KeyV), t0));
        quiet(&ptt.handle(InputEvent::key_down(Code::ControlLeft), t0));
        assert!(!ptt.is_transmitting());
        assert_eq!(
            ptt.handle(InputEvent::key_down(Code::ShiftRight), t0),
            vec![PttEvent::Pressed],
        );
        quiet(&ptt.handle(InputEvent::key_down(Code::MetaLeft), t0));
        assert!(ptt.is_transmitting());
        quiet(&ptt.handle(InputEvent::key_up(Code::ShiftRight), t0));
        assert_eq!(ptt.poll(t0 + DEBOUNCE), Some(PttEvent::Released));

        let mut other = machine(binding, Duration::ZERO);
        let again = Instant::now();
        quiet(&other.handle(InputEvent::key_down(Code::ControlRight), again));
        quiet(&other.handle(InputEvent::key_down(Code::ShiftLeft), again));
        assert_eq!(
            other.handle(InputEvent::key_down(Code::KeyV), again),
            vec![PttEvent::Pressed],
        );
    }

    #[test]
    fn modifier_key_binding_is_side_specific() {
        let mut ptt = machine(Binding::key(Code::ControlLeft), Duration::ZERO);
        let t0 = Instant::now();
        quiet(&ptt.handle(InputEvent::key_down(Code::ControlRight), t0));
        assert_eq!(
            ptt.handle(InputEvent::key_down(Code::ControlLeft), t0),
            vec![PttEvent::Pressed],
        );
    }

    #[test]
    fn key_repeat_does_not_emit_another_press() {
        let mut ptt = machine(Binding::key(Code::F13), Duration::ZERO);
        let t0 = Instant::now();
        assert_eq!(
            ptt.handle(InputEvent::key_down(Code::F13), t0),
            vec![PttEvent::Pressed],
        );
        quiet(&ptt.handle(
            InputEvent::key_down(Code::F13),
            t0 + Duration::from_millis(30),
        ));
        quiet(&ptt.handle(
            InputEvent::key_down(Code::F13),
            t0 + Duration::from_millis(60),
        ));
        assert!(ptt.is_transmitting());
        let up_at = t0 + Duration::from_millis(90);
        quiet(&ptt.handle(InputEvent::key_up(Code::F13), up_at));
        assert_eq!(ptt.poll(up_at + DEBOUNCE), Some(PttEvent::Released));
    }
}
