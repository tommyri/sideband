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

// A receive ceiling, never an assumed path MTU. Discovery starts at Quinn's
// safe 1200-byte default and only grows after acknowledged probes. This lets
// capable LAN/loopback links coalesce frames without delaying audio or changing
// the 1150-byte per-frame payload limit. Smaller peers/paths remain supported.
const MAX_UDP_PAYLOAD: u16 = 8192;

fn bind_endpoint(
    address: std::net::SocketAddr,
    server: Option<quinn::ServerConfig>,
) -> Result<quinn::Endpoint, Error> {
    let mut config = quinn::EndpointConfig::default();
    config
        .max_udp_payload_size(MAX_UDP_PAYLOAD)
        .map_err(|_| Error::Config("invalid UDP receive ceiling"))?;
    let socket = if server.is_none() && address.is_ipv6() {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        // Match Quinn's client constructor: try dual stack before binding so
        // mapped IPv4 destinations work on hosts whose default is V6ONLY.
        let _ = socket.set_only_v6(false);
        socket.bind(&address.into())?;
        std::net::UdpSocket::from(socket)
    } else {
        std::net::UdpSocket::bind(address)?
    };
    socket.set_nonblocking(true)?;
    Ok(quinn::Endpoint::new(
        config,
        server,
        socket,
        std::sync::Arc::new(quinn::TokioRuntime),
    )?)
}

fn transport_config(client: bool) -> quinn::TransportConfig {
    let mut config = quinn::TransportConfig::default();
    let mut discovery = quinn::MtuDiscoveryConfig::default();
    discovery.upper_bound(MAX_UDP_PAYLOAD);
    config.mtu_discovery_config(Some(discovery));
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_batched_udp_io() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let state = quinn_udp::UdpSocketState::new((&socket).into()).unwrap();
        assert!(
            state.max_gso_segments() > 1,
            "macOS socket batching must stay enabled"
        );
    }

    #[test]
    fn sits_on_proto() {
        assert_eq!(crate::foundation(), ["ss-core", "ss-proto"]);
    }
}

/// Public-network address and hostname policy for untrusted URLs.
pub mod public_address;
