//! macOS microphone permission.
//!
//! CoreAudio does not fail when the user has denied the microphone. The
//! stream opens, the callback runs, and every sample is zero. The permission
//! has to be read from AVFAudio before the stream is opened.

use objc2::runtime::AnyClass;
use objc2_avf_audio::{AVAudioApplication, AVAudioApplicationRecordPermission};

use crate::device::MicrophoneAccess;

/// Read the recorded microphone decision for this process. Never prompts.
///
/// `AVAudioApplication` exists from macOS 14. On macOS 13 the answer is
/// [`MicrophoneAccess::Unknown`].
pub(crate) fn microphone_access() -> MicrophoneAccess {
    if AnyClass::get(c"AVAudioApplication").is_none() {
        return MicrophoneAccess::Unknown;
    }
    // SAFETY: the class is registered (checked above). `sharedInstance` takes
    // no arguments and returns the retained process-wide singleton.
    let application = unsafe { AVAudioApplication::sharedInstance() };
    // SAFETY: `recordPermission` is a read-only property getter on a valid
    // instance. It returns the stored decision and does not show a prompt.
    let permission = unsafe { application.recordPermission() };
    match permission {
        AVAudioApplicationRecordPermission::Granted => MicrophoneAccess::Granted,
        AVAudioApplicationRecordPermission::Denied => MicrophoneAccess::Denied,
        AVAudioApplicationRecordPermission::Undetermined => MicrophoneAccess::Undetermined,
        _ => MicrophoneAccess::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::microphone_access;

    #[test]
    fn reading_the_permission_does_not_panic() {
        // The value depends on the machine and the parent process. The test
        // checks that the lookup works on this macOS version.
        let _ = microphone_access();
    }
}
