//! A negotiated connection and its control-stream ownership.

use std::time::Duration;

use crate::{
    Error,
    control::{ClientStream, ControlStream, ServerStream},
};
use bytes::Bytes;

/// Snapshot of Quinn's transport diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionStats {
    /// Smoothed round-trip time.
    pub rtt: Duration,
    /// Packets declared lost by QUIC (not an audio-frame loss counter).
    pub lost_packets: u64,
    /// Congestion window in bytes.
    pub congestion_window: u64,
}

/// A connection retaining its endpoint and allowing one control stream.
/// Low-level QUIC access is provided for future unidirectional video streams.
pub struct Connection {
    pub(crate) inner: quinn::Connection,
    pub(crate) endpoint: quinn::Endpoint,
    pub(crate) is_client: bool,
    pub(crate) control_taken: bool,
}

impl Connection {
    pub(crate) fn new(
        inner: quinn::Connection,
        endpoint: quinn::Endpoint,
        is_client: bool,
    ) -> Result<Self, Error> {
        if inner.max_datagram_size().is_none() {
            inner.close(1_u32.into(), b"datagrams required");
            return Err(Error::DatagramsRequired);
        }
        Ok(Self {
            inner,
            endpoint,
            is_client,
            control_taken: false,
        })
    }

    /// Opens the only client-initiated control stream. Sending the first frame
    /// makes it visible to the server; the application sends `Hello` first.
    pub async fn open_control(&mut self) -> Result<ClientStream, Error> {
        if !self.is_client {
            return Err(Error::Config("only clients open control streams"));
        }
        if self.control_taken {
            return Err(Error::ControlAlreadyTaken);
        }
        let (send, recv) = self.inner.open_bi().await?;
        self.control_taken = true;
        Ok(ControlStream::new(send, recv, self.inner.clone()))
    }

    /// Accepts the only control stream on the server side.
    pub async fn accept_control(&mut self) -> Result<ServerStream, Error> {
        if self.is_client {
            return Err(Error::Config("only servers accept control streams"));
        }
        if self.control_taken {
            return Err(Error::ControlAlreadyTaken);
        }
        let (send, recv) = self.inner.accept_bi().await?;
        self.control_taken = true;
        Ok(ControlStream::new(send, recv, self.inner.clone()))
    }

    /// Sends opaque media without waiting; Quinn may drop older queued datagrams.
    pub fn send_datagram(&self, bytes: Bytes) -> Result<(), quinn::SendDatagramError> {
        self.inner.send_datagram(bytes)
    }

    /// Receives one opaque datagram. Delivery is unreliable and unordered.
    pub async fn read_datagram(&self) -> Result<Bytes, quinn::ConnectionError> {
        self.inner.read_datagram().await
    }

    /// Current path-dependent datagram limit, which may change after migration.
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.inner.max_datagram_size()
    }

    /// Reads current RTT, loss, and congestion-window counters.
    pub fn stats(&self) -> ConnectionStats {
        let path = self.inner.stats().path;
        ConnectionStats {
            rtt: self.inner.rtt(),
            lost_packets: path.lost_packets,
            congestion_window: path.cwnd,
        }
    }

    /// Rebinds the client endpoint to a replacement UDP socket. Quinn validates
    /// the new path before switching traffic, preserving streams and datagrams.
    pub fn rebind(&self, socket: std::net::UdpSocket) -> Result<(), Error> {
        if !self.is_client {
            return Err(Error::Config("only clients migrate connections"));
        }
        self.endpoint.rebind(socket)?;
        Ok(())
    }

    /// Access for future video streams; callers must not open extra bi streams.
    pub const fn quic(&self) -> &quinn::Connection {
        &self.inner
    }

    /// Closes immediately with an application code and reason.
    pub fn close(&self, code: u32, reason: &[u8]) {
        self.inner.close(code.into(), reason);
    }

    /// Waits for endpoint connections to drain after closing them.
    pub async fn wait_idle(&self) {
        self.endpoint.wait_idle().await;
    }
}
