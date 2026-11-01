//! Push-to-talk bindings and the platform-independent press/release model.
//!
//! Decision D8: each operating system gets a raw global listener (Win32 Raw
//! Input, a listen-only `CGEventTap`, X11 XInput2, the XDG GlobalShortcuts
//! portal, or opt-in evdev). Hotkey registration is not used, because it
//! swallows the key and cannot bind mouse buttons.
//!
//! [`Backend::detect`] selects the native listener on macOS and the mock on
//! Windows/Linux until their backends land. No platform feature flags are
//! required. Backends report [`InputEvent`] edges; [`PttStateMachine`] turns
//! them into [`PttEvent`]s.
//!
//! `unsafe` is allowed in this crate. The platform backends are FFI
//! (Win32 hooks, `CGEventTap`, X11, evdev). Every `unsafe` block needs a
//! `// SAFETY:` comment.
#![allow(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod backend;
mod binding;
mod error;
mod listener;
#[cfg(target_os = "macos")]
pub mod macos;
mod mock;
mod state;

#[doc(inline)]
pub use crate::{
    backend::Backend,
    binding::{
        Binding, CaptureSession, Code, HostPlatform, InputEvent, ModifierSet, MouseButton, Trigger,
    },
    error::{Error, Permission},
    listener::{Caps, CapturedBinding, Handle, Handler, Listener},
    mock::MockListener,
    state::{DEBOUNCE, DEFAULT_RELEASE_DELAY, PttEvent, PttStateMachine},
};

/// Package name. Dependents reference this so the crate graph stays linked.
pub const NAME: &str = "ss-input";

/// Crates this layer is built on.
#[must_use]
pub const fn foundation() -> &'static [&'static str] {
    &[ss_core::NAME]
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(crate::NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn sits_on_core() {
        assert_eq!(crate::foundation(), ["ss-core"]);
    }
}
