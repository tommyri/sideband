//! Arming for a global push-to-talk key.
//!
//! A global key can already be held when a call starts, or a listener can
//! miss a release. The key opens the gate only for a press that follows a
//! release seen for the same call, or for a call the host's periodic check
//! has seen before the press.

/// What the host applies to the call after a key event.
#[derive(Debug, PartialEq, Eq)]
pub enum PttAction {
    /// Close the gate now, skipping the release tail.
    Release,
    /// Set the global half of the combined PTT gate.
    Set(bool),
}

/// Tracks whether a global PTT press may open the gate. `session` is any
/// value that identifies one call.
#[derive(Debug, Default)]
pub struct PttArming {
    session: Option<usize>,
    armed: bool,
}

impl PttArming {
    /// A key event for the call identified by `session`.
    pub fn key(&mut self, session: usize, connected: bool, pressed: bool) -> PttAction {
        if pressed && !connected {
            self.armed = false;
            return PttAction::Release;
        }
        if self.session != Some(session) {
            self.session = Some(session);
            self.armed = false;
        }
        if !pressed {
            self.armed = true;
        }
        PttAction::Set(pressed && self.armed)
    }

    /// The periodic check. A call it has not seen before is armed.
    pub fn tick(&mut self, session: Option<usize>) {
        if session != self.session {
            self.session = session;
            self.armed = true;
        }
    }

    /// After a listener failure, require a fresh press.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptt_opens_only_for_presses_after_the_call_is_armed() {
        let mut ptt = PttArming::default();
        // A press for a call the periodic check has not seen yet stays closed
        // until the key is released.
        assert_eq!(ptt.key(1, true, true), PttAction::Set(false));
        assert_eq!(ptt.key(1, true, false), PttAction::Set(false));
        assert_eq!(ptt.key(1, true, true), PttAction::Set(true));
        assert_eq!(ptt.key(1, true, false), PttAction::Set(false));
        // A new call seen by the tick is armed for the next press.
        ptt.tick(Some(2));
        assert_eq!(ptt.key(2, true, true), PttAction::Set(true));
        // A key already down when the call changes must be released first.
        assert_eq!(ptt.key(3, true, true), PttAction::Set(false));
        assert_eq!(ptt.key(3, true, false), PttAction::Set(false));
        assert_eq!(ptt.key(3, true, true), PttAction::Set(true));
        ptt.tick(Some(3));
        assert_eq!(ptt.key(3, true, true), PttAction::Set(true));
    }

    #[test]
    fn ptt_refused_while_disconnected_or_after_failure_needs_a_fresh_press() {
        let mut ptt = PttArming::default();
        ptt.tick(Some(1));
        assert_eq!(ptt.key(1, false, true), PttAction::Release);
        assert_eq!(ptt.key(1, true, true), PttAction::Set(false));
        assert_eq!(ptt.key(1, true, false), PttAction::Set(false));
        assert_eq!(ptt.key(1, true, true), PttAction::Set(true));
        // Releases always close, even while disconnected.
        assert_eq!(ptt.key(1, false, false), PttAction::Set(false));
        ptt.disarm();
        assert_eq!(ptt.key(1, true, true), PttAction::Set(false));
        assert_eq!(ptt.key(1, true, false), PttAction::Set(false));
        assert_eq!(ptt.key(1, true, true), PttAction::Set(true));
        // Leaving the call and joining again re-arms through the tick.
        ptt.tick(None);
        ptt.tick(Some(1));
        assert_eq!(ptt.key(1, true, true), PttAction::Set(true));
    }
}
