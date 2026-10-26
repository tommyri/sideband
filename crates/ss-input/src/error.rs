//! Failures a listener can return to the settings UI or the voice session.

use std::{fmt, sync::PoisonError};

use thiserror::Error;

/// Grant a platform listener needs before it can observe global input.
///
/// macOS Input Monitoring is the listen-only `CGEventTap` permission.
/// The Linux `input` group is what opt-in evdev requires. Neither is a
/// hotkey registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Permission {
    /// macOS Input Monitoring (TCC listen-event access).
    InputMonitoring,
    /// Membership in the Linux `input` group, for the evdev backend.
    InputGroup,
}

impl fmt::Display for Permission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InputMonitoring => "Input Monitoring",
            Self::InputGroup => "Linux input group",
        })
    }
}

/// Errors from starting a listener, updating it, or capturing a binding.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum Error {
    /// [`crate::Listener::start`] was called while a handle is already live.
    #[error("input listener is already running")]
    AlreadyRunning,

    /// The handle was used after [`crate::Handle::stop`], or before start.
    #[error("input listener is not running")]
    NotRunning,

    /// A second [`crate::Listener::capture_next`] started before the first finished.
    #[error("binding capture is already in progress")]
    CaptureInProgress,

    /// Capture waited until its timeout without a completed gesture.
    #[error("timed out waiting for a key or mouse button")]
    CaptureTimeout,

    /// [`crate::Listener::cancel_capture`] unblocked a waiting capture.
    #[error("binding capture was cancelled")]
    CaptureCancelled,

    /// macOS or evdev refused to listen until the user grants `permission`.
    ///
    /// `settings_url` is an OS settings deep link when the backend knows one.
    /// The UI opens it; this crate does not.
    #[error("permission required ({permission})")]
    PermissionRequired {
        /// Which grant is missing.
        permission: Permission,
        /// System settings URL, or `None` when the OS has no pane to open.
        settings_url: Option<String>,
    },

    /// A panic while the listener lock was held poisoned that lock.
    #[error("input listener lock was poisoned")]
    LockPoisoned,
}

impl<T> From<PoisonError<T>> for Error {
    fn from(_error: PoisonError<T>) -> Self {
        Self::LockPoisoned
    }
}
