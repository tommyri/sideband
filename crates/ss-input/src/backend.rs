//! Runtime selection of the input backend.
//!
//! [`Backend::detect`] is the function later tasks replace. It is not behind
//! a cargo feature, so this crate links on Windows, macOS, and Linux today.

use std::{fmt, time::Duration};

use tracing::debug;

use crate::{
    error::Error,
    listener::{Caps, CapturedBinding, Handle, Handler, Listener},
    mock::MockListener,
};

/// The input backend for this process.
///
/// Construct it with [`Self::detect`]. The value reports [`Caps`] so a
/// settings screen can explain permission prompts, missing mouse buttons,
/// and shortcuts the compositor owns.
pub struct Backend {
    listener: Box<dyn Listener>,
}

impl Backend {
    /// Select the listener for this process.
    ///
    /// This build always returns [`MockListener`]. The body of this function
    /// is what later tasks replace:
    ///
    /// - Windows: Raw Input, with a low-level hook fallback
    /// - macOS: a listen-only `CGEventTap`
    /// - Linux: the XDG GlobalShortcuts portal when `WAYLAND_DISPLAY` is set,
    ///   otherwise X11 XInput2, with evdev only when the user opts in
    ///
    /// That is decision D8. Hotkey registration is intentionally not used:
    /// it swallows the key and cannot bind mouse buttons. No backend cargo
    /// feature is required to compile.
    #[must_use]
    pub fn detect() -> Self {
        debug!("selected the mock input backend");
        Self {
            listener: Box::new(MockListener::new(Caps::mock())),
        }
    }

    /// Capabilities of the selected listener.
    #[must_use]
    pub fn capabilities(&self) -> Caps {
        self.listener.capabilities()
    }

    /// Start the selected listener.
    ///
    /// # Errors
    ///
    /// See [`Listener::start`].
    pub fn start(&self, handler: Handler) -> Result<Handle, Error> {
        self.listener.start(handler)
    }

    /// Capture the next binding, or report that the compositor owns it.
    ///
    /// # Errors
    ///
    /// See [`Listener::capture_next`].
    pub fn capture_next(&self, timeout: Duration) -> Result<CapturedBinding, Error> {
        self.listener.capture_next(timeout)
    }

    /// Unblock [`Self::capture_next`].
    ///
    /// # Errors
    ///
    /// See [`Listener::cancel_capture`].
    pub fn cancel_capture(&self) -> Result<(), Error> {
        self.listener.cancel_capture()
    }
}

impl fmt::Debug for Backend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Backend")
            .field("capabilities", &self.capabilities())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Backend;
    use crate::{
        binding::{Binding, Code},
        error::Error,
        listener::Caps,
    };

    #[test]
    fn detect_reports_mock_limits_and_starts() {
        let backend = Backend::detect();
        let caps = backend.capabilities();
        assert!(caps.mouse_buttons);
        assert!(caps.passthrough);
        assert!(caps.needs_permission.is_none());
        assert!(!caps.binding_set_by_system);
        assert_eq!(caps.limitations, [Caps::MOCK_LIMITATION]);

        let handle = backend.start(Box::new(|_event| {})).unwrap();
        handle.update_bindings(&[Binding::key(Code::F13)]).unwrap();
        assert_eq!(
            backend.capture_next(Duration::ZERO).unwrap_err(),
            Error::AlreadyRunning,
        );
        handle.stop().unwrap();
        assert_eq!(
            backend.capture_next(Duration::ZERO).unwrap_err(),
            Error::CaptureTimeout,
        );
    }

    #[test]
    fn none_caps_explain_an_unavailable_session() {
        let caps = Caps::none();
        assert!(!caps.mouse_buttons);
        assert!(!caps.passthrough);
        assert!(caps.needs_permission.is_none());
        assert!(!caps.binding_set_by_system);
        assert_eq!(caps.limitations, [Caps::UNAVAILABLE_LIMITATION]);
    }
}
