//! Listener trait shared by the mock and the later operating-system backends.
//!
//! Backends report edges. They do not decide when transmit opens. That is
//! [`crate::PttStateMachine`], so debounce and the release delay match on
//! every platform.

use std::{fmt, time::Duration};

use tracing::debug;

use crate::{
    binding::Binding,
    error::{Error, Permission},
    state::PttEvent,
};

/// Callback for each transmit edge.
///
/// The mock runs it on the thread that injected the event. Operating-system
/// backends run it on their input thread. Send controls from another thread;
/// synchronous controls called from a native handler return ReentrantControl.
/// Keep it short and do not
/// panic. Shared state captured by the closure needs its own synchronization;
/// the callback is [`Send`] and [`Sync`] so it can cross that thread.
pub type Handler = Box<dyn Fn(PttEvent) + Send + Sync + 'static>;

/// What a backend can actually do, for the settings screen.
///
/// The UI must not offer a control the flags rule out: no mouse capture when
/// `mouse_buttons` is false, and no in-app key swallow when
/// `binding_set_by_system` is set.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Caps {
    /// Mouse buttons can be bindings.
    ///
    /// False for the Wayland GlobalShortcuts portal, which only binds keys
    /// the compositor offers.
    pub mouse_buttons: bool,
    /// The focused application still receives the event.
    ///
    /// Listen-only backends from decision D8 set this. Some compositors
    /// consume a portal shortcut and set it false.
    pub passthrough: bool,
    /// Grant required before [`Listener::start`] can succeed.
    ///
    /// `None` when the backend does not need a special permission.
    pub needs_permission: Option<Permission>,
    /// The compositor owns the shortcut.
    ///
    /// Capture must not swallow a key. The settings UI sends the user to
    /// system settings instead.
    pub binding_set_by_system: bool,
    /// Sentences a settings screen can show as-is.
    ///
    /// Empty when there is nothing to explain. Windows UIPI and compositor
    /// notes append here, including dynamic text such as a game name.
    pub limitations: Vec<String>,
}

impl Caps {
    /// Shown by [`Self::mock`].
    pub const MOCK_LIMITATION: &'static str = concat!(
        "No operating-system listener is linked, so global push-to-talk cannot ",
        "see keys or mouse buttons. Platform backends are selected at runtime ",
        "in later tasks; they are not cargo features."
    );

    /// Shown by [`Self::none`].
    pub const UNAVAILABLE_LIMITATION: &'static str = concat!(
        "Push-to-talk shortcuts are unavailable in this session. ",
        "Voice activation is the transmit mode that still works."
    );

    /// No keys, no mouse, no passthrough.
    ///
    /// Linux sessions without a portal and without X11 return this so the UI
    /// can fall back to voice activation.
    #[must_use]
    pub fn none() -> Self {
        Self {
            mouse_buttons: false,
            passthrough: false,
            needs_permission: None,
            binding_set_by_system: false,
            limitations: vec![Self::UNAVAILABLE_LIMITATION.to_owned()],
        }
    }

    /// Capabilities of [`crate::MockListener`].
    ///
    /// Mouse buttons and passthrough are reported as available because the
    /// mock does not grab input. The limitation string says no operating-system
    /// listener is linked.
    #[must_use]
    pub fn mock() -> Self {
        Self {
            mouse_buttons: true,
            passthrough: true,
            needs_permission: None,
            binding_set_by_system: false,
            limitations: vec![Self::MOCK_LIMITATION.to_owned()],
        }
    }

    /// Append one sentence for the settings screen.
    #[must_use]
    pub fn with_limitation(mut self, limitation: impl Into<String>) -> Self {
        self.limitations.push(limitation.into());
        self
    }
}

/// Result of asking the user for a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CapturedBinding {
    /// The user completed a gesture.
    ///
    /// The listener observed it. It did not register a hotkey. The focused
    /// application still receives the key when [`Caps::passthrough`] is set.
    Binding(Binding),
    /// [`Caps::binding_set_by_system`] is set.
    ///
    /// No key was consumed. Open the compositor's shortcut settings instead.
    SetBySystem,
}

/// A running listener.
///
/// Dropping the handle stops the listener and force-releases a held chord so
/// transmit cannot stick. [`Self::stop`] does the same and reports lock errors.
pub struct Handle {
    ops: Box<dyn ListenerControl>,
}

impl Handle {
    pub(crate) fn new(ops: Box<dyn ListenerControl>) -> Self {
        Self { ops }
    }

    /// Check for a runtime failure, such as revoked Input Monitoring or secure
    /// input. The app should poll this and recheck capabilities on focus.
    pub fn status(&self) -> Result<(), Error> {
        self.ops.status()
    }

    /// Replace the chords the listener watches.
    ///
    /// A chord that is no longer held starts the release delay. A newly
    /// matching chord emits press.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotRunning`] after stop, and [`Error::LockPoisoned`]
    /// if a callback panicked while the listener lock was held.
    pub fn update_bindings(&self, bindings: &[Binding]) -> Result<(), Error> {
        self.ops.update_bindings(bindings)
    }

    /// Change how long transmit stays open after the physical release.
    ///
    /// The debounce floor still applies. See [`crate::DEBOUNCE`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotRunning`] after stop, and [`Error::LockPoisoned`]
    /// if the listener lock was poisoned.
    pub fn set_release_delay(&self, delay: Duration) -> Result<(), Error> {
        self.ops.set_release_delay(delay)
    }

    /// Drop a stuck chord immediately.
    ///
    /// Focus loss, sleep, and a disabled event tap call this. Keys that were
    /// down stay suppressed until their matching up.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotRunning`] after stop, and [`Error::LockPoisoned`]
    /// if the listener lock was poisoned.
    pub fn force_release(&self) -> Result<(), Error> {
        self.ops.force_release()
    }

    /// Stop listening and force-release a held chord.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotRunning`] if the listener was already stopped, and
    /// [`Error::LockPoisoned`] if the listener lock was poisoned.
    pub fn stop(self) -> Result<(), Error> {
        self.ops.stop()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.ops.stop();
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Handle").finish_non_exhaustive()
    }
}

/// Platform input listener.
///
/// [`crate::Backend::detect`] boxes one. The mock and native backends share
/// this contract. Do not override [`Self::capture_next`]: the default refuses to
/// observe keys when the compositor owns the binding.
pub trait Listener: Send + Sync {
    /// What this backend can bind, and what the UI should explain.
    fn capabilities(&self) -> Caps;

    /// Start reporting transmit edges to `handler`.
    ///
    /// Bindings start empty. Call [`Handle::update_bindings`] before expecting
    /// events.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AlreadyRunning`] if a handle is already live,
    /// [`Error::CaptureInProgress`] if capture has the listener, and
    /// [`Error::PermissionRequired`] when the operating system has not granted
    /// access. [`Error::LockPoisoned`] means a previous callback panicked.
    fn start(&self, handler: Handler) -> Result<Handle, Error>;

    /// Wait until the user finishes a binding gesture, or until `timeout`.
    ///
    /// When [`Caps::binding_set_by_system`] is set, this returns
    /// [`CapturedBinding::SetBySystem`] immediately and does not consume a key.
    ///
    /// # Errors
    ///
    /// See [`Self::capture_gesture`]. The compositor path does not fail.
    fn capture_next(&self, timeout: Duration) -> Result<CapturedBinding, Error> {
        if self.capabilities().binding_set_by_system {
            debug!("binding capture left to the compositor");
            return Ok(CapturedBinding::SetBySystem);
        }
        self.capture_gesture(timeout).map(CapturedBinding::Binding)
    }

    /// Observe the next gesture.
    ///
    /// Applications call [`Self::capture_next`]. Backends implement this.
    /// It runs only when the compositor does not own the binding.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AlreadyRunning`] if the listener is started,
    /// [`Error::CaptureInProgress`] if another capture is waiting,
    /// [`Error::CaptureTimeout`] if `timeout` elapses,
    /// [`Error::CaptureCancelled`] after [`Self::cancel_capture`], and
    /// [`Error::LockPoisoned`] if the listener lock was poisoned.
    fn capture_gesture(&self, timeout: Duration) -> Result<Binding, Error>;

    /// Unblock [`Self::capture_next`].
    ///
    /// Safe to call before capture starts. The next capture then returns
    /// [`Error::CaptureCancelled`] instead of waiting.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LockPoisoned`] if the listener lock was poisoned.
    fn cancel_capture(&self) -> Result<(), Error>;
}

pub(crate) trait ListenerControl: Send + Sync {
    fn status(&self) -> Result<(), Error> {
        Ok(())
    }
    fn update_bindings(&self, bindings: &[Binding]) -> Result<(), Error>;
    fn set_release_delay(&self, delay: Duration) -> Result<(), Error>;
    fn force_release(&self) -> Result<(), Error>;
    fn stop(&self) -> Result<(), Error>;
}
