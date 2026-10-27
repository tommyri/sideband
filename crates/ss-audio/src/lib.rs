//! Voice capture, encode, receive, and playback.
//!
//! Device I/O, resampling, the Opus wrapper, the jitter buffer, and the
//! mixer live here. Nothing in this crate opens a socket.

pub mod codec;
pub mod device;
pub mod jitter;
pub mod mixer;
pub mod receive;
pub mod resample;

/// Package name. Dependents reference this so the crate graph stays linked.
pub const NAME: &str = "ss-audio";

/// Crates this layer is built on.
#[must_use]
pub const fn foundation() -> &'static [&'static str] {
    &[ss_core::NAME]
}

#[allow(unsafe_code)]
pub mod platform {
    //! Operating-system audio backends.
    //!
    //! `unsafe` is allowed in this module for CoreAudio, WASAPI, and
    //! PipeWire bindings. Every `unsafe` block needs a `// SAFETY:` comment.
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
