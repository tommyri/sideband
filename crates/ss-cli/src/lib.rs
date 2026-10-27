//! Headless SocialSpeak client.
//!
//! `ss-cli connect` places a voice call against the spike server, measures
//! mouth-to-ear latency, and can impair or multiply the send path. The desktop
//! app does not link this crate.

mod audio;
mod bots;
mod call;
mod channel;
mod graph;
mod impair;
mod measure;
mod terminal;
mod wav;

pub use call::{CallError, Gate, Options, Report, run};
pub use channel::{ChannelError, channel_from_arg};
pub use impair::{ImpairError, Impairment};

/// Package name. Dependents reference this so the crate graph stays linked.
pub const NAME: &str = "ss-cli";

#[cfg(test)]
mod it;

#[cfg(test)]
mod tests {
    #[test]
    fn links_client_core() {
        assert_eq!(ss_client_core::NAME, "ss-client-core");
    }
}
