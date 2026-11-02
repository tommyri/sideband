//! Runtime selection of the input backend.
//!
//! [`Backend::detect`] selects the native Windows or macOS listener and keeps
//! the mock on Linux pending T-050. No platform cargo features.

use std::{fmt, time::Duration};

use tracing::debug;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use crate::mock::MockListener;

use crate::{
    error::Error,
    listener::{Caps, CapturedBinding, Handle, Handler, Listener},
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
    /// Windows uses Raw Input on a message-only window. macOS uses a
    /// listen-only session event tap with Input Monitoring. Linux still uses
    /// the mock pending T-050.
    /// Detection itself never prompts; start/capture request permission.
    ///
    /// That is decision D8. Hotkey registration is intentionally not used:
    /// it swallows the key and cannot bind mouse buttons. No backend cargo
    /// feature is required to compile.
    #[must_use]
    pub fn detect() -> Self {
        #[cfg(target_os = "macos")]
        let listener: Box<dyn Listener> = Box::new(crate::macos::MacListener::new());
        #[cfg(target_os = "windows")]
        let listener: Box<dyn Listener> = Box::new(crate::windows::WinListener::new());
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let listener: Box<dyn Listener> = Box::new(MockListener::new(Caps::mock()));
        debug!(os = std::env::consts::OS, "selected input backend");
        Self { listener }
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
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    use std::time::Duration;

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    use super::Backend;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    use crate::{
        binding::{Binding, Code},
        error::Error,
        listener::Caps,
    };

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
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
        let caps = crate::Caps::none();
        assert!(!caps.mouse_buttons);
        assert!(!caps.passthrough);
        assert!(caps.needs_permission.is_none());
        assert!(!caps.binding_set_by_system);
        assert_eq!(caps.limitations, [crate::Caps::UNAVAILABLE_LIMITATION]);
    }
}
