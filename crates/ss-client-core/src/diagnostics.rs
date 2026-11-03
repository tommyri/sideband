//! On-demand diagnostics shared by desktop and headless hosts. No identities or secrets.
use serde::Serialize;

/// Current QUIC path counters; loss is packets, not audio frames.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportDiagnostics {
    /// Smoothed network round trip, in milliseconds.
    pub rtt_ms: f64,
    /// Cumulative lost packets on the current path.
    pub lost_packets: u64,
    /// Cumulative packets sent on the current path.
    pub sent_packets: u64,
    /// Current congestion window, in bytes.
    pub congestion_window_bytes: u64,
    /// Largest supported UDP payload, in bytes; not application datagram size.
    pub path_mtu_bytes: u16,
    /// QUIC application's current datagram ceiling, in bytes.
    pub max_datagram_bytes: Option<usize>,
}
impl crate::Client {
    /// Missing during disconnect/reconnect; never substitutes stale zero counters.
    pub fn diagnostics(&self) -> Option<TransportDiagnostics> {
        let connection = self.media.quic()?;
        if connection.close_reason().is_some() {
            return None;
        }
        let path = connection.stats().path;
        Some(TransportDiagnostics {
            rtt_ms: connection.rtt().as_secs_f64() * 1000.0,
            lost_packets: path.lost_packets,
            sent_packets: path.sent_packets,
            congestion_window_bytes: path.cwnd,
            path_mtu_bytes: path.current_mtu,
            max_datagram_bytes: connection.max_datagram_size(),
        })
    }
}
