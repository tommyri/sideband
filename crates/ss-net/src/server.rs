//! Server endpoint configuration; authentication belongs to `ss-server`.

use rustls::server::ResolvesServerCert;

use crate::{
    Connection, Error,
    tls::{CertResolver, TlsConfig},
};
use std::{net::SocketAddr, sync::Arc};

/// Bound UDP endpoint, normally on port 443.
#[derive(Clone)]
pub struct Endpoint(quinn::Endpoint);

/// One incoming peer whose handshake has not yet been awaited.
/// Dispatch these into bounded per-peer tasks so a stalled handshake cannot
/// block the endpoint's accept loop. Dropping one refuses the connection.
pub struct Incoming {
    inner: quinn::Incoming,
    endpoint: quinn::Endpoint,
}

impl Incoming {
    /// Completes this peer's handshake and validates datagram support.
    /// A failed handshake affects only this peer, not the endpoint.
    pub async fn accept(self) -> Result<Connection, Error> {
        Connection::new(self.inner.await?, self.endpoint, false)
    }

    /// Rejects a peer without waiting for its handshake, for example when the
    /// application's concurrent-handshake limit has been reached.
    pub fn refuse(self) {
        self.inner.refuse();
    }

    /// Peer socket address, available before completing the handshake.
    pub fn remote_address(&self) -> SocketAddr {
        self.inner.remote_address()
    }
}

impl Endpoint {
    /// Binds with SocialSpeak ALPN, datagrams, and future video stream capacity.
    pub fn bind(addr: SocketAddr, tls: TlsConfig) -> Result<Self, Error> {
        Self::bind_resolver(addr, Arc::new(CertResolver::new(tls)?))
    }

    /// Binds with a resolver shared with later certificate reloads.
    /// [`CertResolver::replace`] updates future handshakes on this socket.
    pub fn bind_resolver(
        addr: SocketAddr,
        resolver: Arc<dyn ResolvesServerCert>,
    ) -> Result<Self, Error> {
        let mut config = crate::tls::server_config(resolver)?;
        config.transport_config(Arc::new(crate::transport_config(false)));
        Ok(Self(crate::bind_endpoint(addr, Some(config))?))
    }

    /// Socket address, including the assigned port when binding port zero.
    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.0.local_addr()
    }

    /// Receives one incoming peer without awaiting its handshake. Dispatch
    /// [`Incoming::accept`] into a bounded per-peer task and continue accepting;
    /// use [`Incoming::refuse`] when at capacity. `None` means shutdown.
    pub async fn accept(&self) -> Option<Incoming> {
        self.0.accept().await.map(|inner| Incoming {
            inner,
            endpoint: self.0.clone(),
        })
    }

    /// Closes the endpoint and its connections.
    pub fn close(&self, code: u32, reason: &[u8]) {
        self.0.close(code.into(), reason);
    }

    /// Waits for closed connections to finish draining.
    pub async fn wait_idle(&self) {
        self.0.wait_idle().await;
    }
}
