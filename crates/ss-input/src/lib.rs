//! Global input listeners for push-to-talk.
//!
//! `unsafe` is allowed in this crate. The platform backends are FFI
//! (Win32 hooks, `CGEventTap`, X11, evdev). Every `unsafe` block needs a
//! `// SAFETY:` comment.
#![allow(unsafe_code)]

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
