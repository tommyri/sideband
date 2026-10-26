//! Incomplete handshakes must not serialize acceptance of unrelated peers.

use std::{future::Future, net::Ipv4Addr, sync::Arc, time::Duration};

use ss_net::{
    client,
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("handshake operation timed out")
}

#[tokio::test]
async fn incomplete_handshake_does_not_block_another_peer() {
    let directory = tempfile::tempdir().unwrap();
    let identity = tls::generate_self_signed(directory.path()).await.unwrap();
    let certificate = identity.certificates[0].clone();
    let fingerprint = tls::fingerprint(certificate.as_ref());
    let server = Endpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), identity).unwrap();
    let server_addr = server.local_addr().unwrap();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate).unwrap();
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![ss_net::ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    let mut stalled_endpoint = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    stalled_endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(crypto)));

    // Forward only the client's Initial packet. Suppressing the server's reply
    // leaves a real QUIC handshake incomplete without sleeps or forged packets.
    let proxy = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let stalled_client = tokio::spawn(
        stalled_endpoint
            .connect(proxy.local_addr().unwrap(), "localhost")
            .unwrap(),
    );
    let mut packet = vec![0; 65_536];
    let (len, _) = within(proxy.recv_from(&mut packet)).await.unwrap();
    proxy.send_to(&packet[..len], server_addr).await.unwrap();

    let incoming = within(server.accept()).await.unwrap();
    assert_eq!(incoming.remote_address(), proxy.local_addr().unwrap());
    let stalled_handshake = tokio::spawn(incoming.accept());
    loop {
        let (_, source) = within(proxy.recv_from(&mut packet)).await.unwrap();
        if source == server_addr {
            break;
        }
    }

    let pins = Arc::new(MemoryPinStore::default());
    pins.set(
        &ServerId::new("127.0.0.1", server_addr.port()).unwrap(),
        fingerprint,
    )
    .unwrap();
    let accept_next = async { server.accept().await.unwrap().accept().await.unwrap() };
    let connect_next = client::connect("127.0.0.1", server_addr.port(), pins);
    let (accepted, connected) = within(async { tokio::join!(accept_next, connect_next) }).await;
    let connected = connected.unwrap();
    assert!(accepted.max_datagram_size().is_some());
    assert!(!stalled_handshake.is_finished());

    connected.close(0, b"test complete");
    server.close(0, b"test complete");
    stalled_endpoint.close(0_u32.into(), b"test complete");
    stalled_handshake.abort();
    stalled_client.abort();
}

#[tokio::test]
async fn incoming_peer_can_be_refused_without_completing_handshake() {
    let directory = tempfile::tempdir().unwrap();
    let identity = tls::generate_self_signed(directory.path()).await.unwrap();
    let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
    let server = Endpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), identity).unwrap();
    let address = server.local_addr().unwrap();
    let pins = Arc::new(MemoryPinStore::default());
    pins.set(
        &ServerId::new("127.0.0.1", address.port()).unwrap(),
        fingerprint,
    )
    .unwrap();
    let refuse = async { server.accept().await.unwrap().refuse() };
    let connect = client::connect("127.0.0.1", address.port(), pins);
    let ((), result) = within(async { tokio::join!(refuse, connect) }).await;
    assert!(matches!(result, Err(ss_net::Error::Connection(_))));
    server.close(0, b"test complete");
    assert!(within(server.accept()).await.is_none());
}
