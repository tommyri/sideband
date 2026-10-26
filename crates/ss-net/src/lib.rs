//! QUIC transport shared by the server and the client.
//!
//! Endpoint setup, trust-on-first-use certificate pinning, the control
//! stream, and media datagram headers live here. No accounts, channels, or
//! forwarding policy.

pub mod client;
mod connection;
pub mod control;
mod error;
pub mod media;
pub mod pins;
pub mod server;
pub mod tls;

pub use connection::{Connection, ConnectionStats};
pub use error::Error;

/// Negotiated application protocol, shared by every endpoint.
pub const ALPN: &[u8] = b"socialspeak/1";
/// Default server UDP port.
pub const DEFAULT_PORT: u16 = 443;

fn transport_config(client: bool) -> quinn::TransportConfig {
    let mut config = quinn::TransportConfig::default();
    config.max_idle_timeout(Some(quinn::VarInt::from_u32(30_000).into()));
    config.keep_alive_interval(None);
    config.datagram_receive_buffer_size(Some(4 * 1024 * 1024));
    config.datagram_send_buffer_size(4 * 1024 * 1024);
    // Only clients initiate the single control stream. Video uses uni streams.
    config.max_concurrent_bidi_streams(u32::from(!client).into());
    config.max_concurrent_uni_streams(64_u32.into());
    config
}

/// Package name. Dependents reference this so the crate graph stays linked.
pub const NAME: &str = "ss-net";

/// Crates this layer is built on.
#[must_use]
pub const fn foundation() -> &'static [&'static str] {
    &[ss_core::NAME, ss_proto::NAME]
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(crate::NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn sits_on_proto() {
        assert_eq!(crate::foundation(), ["ss-core", "ss-proto"]);
    }
}
