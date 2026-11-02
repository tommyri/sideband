//! macOS microphone permission.
//!
//! CoreAudio does not fail when the user has denied the microphone. The
//! stream opens, the callback runs, and every sample is zero. The permission
//! has to be read from AVFAudio before the stream is opened.

use objc2::{msg_send, runtime::AnyClass};
use objc2_avf_audio::{AVAudioApplication, AVAudioApplicationRecordPermission};

use crate::device::{MicrophoneAccess, MicrophoneMode, MicrophoneModeStatus};

// Referencing the class through the Objective-C runtime still requires the
// framework to be linked so macOS registers AVCaptureDevice at process start.
#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

/// Read the recorded microphone decision for this process. Never prompts.
///
/// `AVAudioApplication` exists from macOS 14. On macOS 13 the answer is
/// [`MicrophoneAccess::Unknown`].
pub(crate) fn microphone_access() -> MicrophoneAccess {
    let Some(class) = AnyClass::get(c"AVAudioApplication") else {
        return MicrophoneAccess::Unknown;
    };
    // SAFETY: the class is registered (checked above) and `sharedInstance`
    // takes no arguments. It may legitimately return null in a headless,
    // unbundled process, so avoid the generated nonnull wrapper here.
    let application: *mut AVAudioApplication = unsafe { msg_send![class, sharedInstance] };
    // SAFETY: a nonnull shared singleton remains owned by AVFAudio for the
    // lifetime of the process; this borrow does not assume ownership.
    let Some(application) = (unsafe { application.as_ref() }) else {
        return MicrophoneAccess::Unknown;
    };
    // SAFETY: `recordPermission` is a read-only property getter on a valid
    // instance. It returns the stored decision and does not show a prompt.
    let permission = unsafe { msg_send![application, recordPermission] };
    match permission {
        AVAudioApplicationRecordPermission::Granted => MicrophoneAccess::Granted,
        AVAudioApplicationRecordPermission::Denied => MicrophoneAccess::Denied,
        AVAudioApplicationRecordPermission::Undetermined => MicrophoneAccess::Undetermined,
        _ => MicrophoneAccess::Unknown,
    }
}

/// Read the user's selected and currently effective Control Center mic modes.
pub(crate) fn microphone_mode_status() -> MicrophoneModeStatus {
    let Some(device) = AnyClass::get(c"AVCaptureDevice") else {
        return unknown_modes();
    };
    // SAFETY: AVCaptureDevice has exposed both parameterless class getters on
    // macOS 12+. They return the NSInteger-backed AVCaptureMicrophoneMode enum
    // and do not mutate capture state.
    let preferred = unsafe { msg_send![device, preferredMicrophoneMode] };
    // SAFETY: same class and read-only API contract as above.
    let active = unsafe { msg_send![device, activeMicrophoneMode] };
    MicrophoneModeStatus {
        preferred: mode(preferred),
        active: mode(active),
    }
}

const fn mode(raw: isize) -> MicrophoneMode {
    match raw {
        0 => MicrophoneMode::Standard,
        1 => MicrophoneMode::WideSpectrum,
        2 => MicrophoneMode::VoiceIsolation,
        _ => MicrophoneMode::Unknown,
    }
}

const fn unknown_modes() -> MicrophoneModeStatus {
    MicrophoneModeStatus {
        preferred: MicrophoneMode::Unknown,
        active: MicrophoneMode::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::{microphone_access, microphone_mode_status};

    #[test]
    fn reading_the_permission_does_not_panic() {
        // The value depends on the machine and the parent process. The test
        // checks that the lookup works on this macOS version.
        let _ = microphone_access();
    }

    #[test]
    fn reading_microphone_modes_does_not_panic() {
        let _ = microphone_mode_status();
    }
}
