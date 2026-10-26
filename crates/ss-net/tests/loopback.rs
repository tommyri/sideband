use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use bytes::BytesMut;
use ss_net::{
    Connection, Error, client,
    control::{ClientControl, ClientStream, ControlError, Reply, ServerStream},
    media::DatagramHeader,
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls::{self, TlsConfig},
};
use ss_proto::{
    frame,
    v1::{self, ClientEnvelope, ServerEnvelope, client_envelope, server_envelope},
};
use tokio::{sync::mpsc, task::JoinHandle};

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("operation timed out")
}

struct Server {
    endpoint: Endpoint,
    incoming: mpsc::Receiver<Connection>,
    worker: JoinHandle<()>,
    fingerprint: tls::Fingerprint,
    certificate: rustls::pki_types::CertificateDer<'static>,
}

impl Server {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self::with_tls(tls::generate_self_signed(dir.path()).await.unwrap())
    }
    fn with_tls(tls: TlsConfig) -> Self {
        let certificate = tls.certificates[0].clone();
        let fingerprint = tls::fingerprint(certificate.as_ref());
        let endpoint = Endpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), tls).unwrap();
        let acceptor = endpoint.clone();
        let (tx, incoming) = mpsc::channel(8);
        let worker = tokio::spawn(async move {
            let mut handshakes = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    incoming = acceptor.accept() => {
                        let Some(incoming) = incoming else { break; };
                        if handshakes.len() >= 8 { incoming.refuse(); continue; }
                        let tx = tx.clone();
                        handshakes.spawn(async move {
                            if let Ok(connection) = incoming.accept().await {
                                let _ = tx.send(connection).await;
                            }
                        });
                    }
                    _ = handshakes.join_next(), if !handshakes.is_empty() => {},
                }
            }
        });
        Self {
            endpoint,
            incoming,
            worker,
            fingerprint,
            certificate,
        }
    }
    fn addr(&self) -> SocketAddr {
        self.endpoint.local_addr().unwrap()
    }
    async fn next(&mut self) -> Connection {
        within(self.incoming.recv()).await.unwrap()
    }
    fn pins(&self) -> Arc<MemoryPinStore> {
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(
            &ServerId::new("127.0.0.1", self.addr().port()).unwrap(),
            self.fingerprint,
        )
        .unwrap();
        pins
    }
    async fn pair(&mut self) -> (Connection, ClientStream, Connection, ServerStream) {
        self.pair_with_pins(self.pins()).await
    }
    async fn pair_with_pins(
        &mut self,
        pins: Arc<MemoryPinStore>,
    ) -> (Connection, ClientStream, Connection, ServerStream) {
        let mut client = within(client::connect("127.0.0.1", self.addr().port(), pins))
            .await
            .unwrap();
        let mut client_stream = client.open_control().await.unwrap();
        client_stream
            .send(&ClientEnvelope {
                request_id: 1,
                msg: Some(client_envelope::Msg::Hello(v1::Hello {
                    protocol_version: 1,
                    ..Default::default()
                })),
            })
            .await
            .unwrap();
        let mut server = self.next().await;
        let mut server_stream = within(server.accept_control()).await.unwrap();
        assert!(matches!(
            within(server_stream.recv()).await.unwrap().msg,
            Some(client_envelope::Msg::Hello(_))
        ));
        server_stream
            .send(&ServerEnvelope {
                request_id: 1,
                msg: Some(server_envelope::Msg::Welcome(v1::Welcome::default())),
            })
            .await
            .unwrap();
        within(client_stream.recv()).await.unwrap();
        (client, client_stream, server, server_stream)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test finished");
        self.worker.abort();
    }
}

fn ping(sent_at: i64) -> client_envelope::Msg {
    client_envelope::Msg::Ping(v1::Ping { sent_at })
}
fn notice(text: &str) -> ServerEnvelope {
    ServerEnvelope {
        request_id: 0,
        msg: Some(server_envelope::Msg::ServerNotice(v1::ServerNotice {
            text: text.to_owned(),
        })),
    }
}
fn ack(id: u32) -> ServerEnvelope {
    ServerEnvelope::ack(id, v1::ack::Result::Empty(v1::Empty {}))
}

#[tokio::test]
async fn tofu_then_ping_pong_and_one_thousand_datagrams() {
    let mut server = Server::new().await;
    let pins = Arc::new(MemoryPinStore::default());
    let untrusted = within(client::connect(
        "127.0.0.1",
        server.addr().port(),
        pins.clone(),
    ))
    .await;
    assert!(
        matches!(untrusted, Err(Error::Untrusted { fingerprint }) if fingerprint == server.fingerprint)
    );
    let id = ServerId::new("127.0.0.1", server.addr().port()).unwrap();
    assert_eq!(pins.get(&id).unwrap(), None);
    pins.set(&id, server.fingerprint).unwrap();
    let (client, mut control, peer, mut server_control) = server.pair_with_pins(pins).await;
    let tls_info = client
        .quic()
        .handshake_data()
        .unwrap()
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .unwrap();
    assert_eq!(tls_info.protocol.as_deref(), Some(ss_net::ALPN));
    assert!(client.max_datagram_size().unwrap() >= 1160);
    assert!(client.stats().congestion_window > 0);
    let exchange = async {
        control
            .send(&ClientEnvelope {
                request_id: 2,
                msg: Some(ping(17)),
            })
            .await
            .unwrap();
        let response = control.recv().await.unwrap();
        assert_eq!(response.request_id, 2);
        assert!(matches!(
            response.msg,
            Some(server_envelope::Msg::Pong(v1::Pong { sent_at: 17, .. }))
        ));
        for seq in 0..1000_u16 {
            let header = DatagramHeader {
                version: 0,
                kind: 0,
                flags: 0,
                track_id: 16_384,
                seq,
                timestamp: u32::from(seq) * 960,
            };
            let mut bytes = BytesMut::new();
            header.encode_into(&mut bytes).unwrap();
            bytes.extend_from_slice(b"opaque opus with redundancy");
            client.send_datagram(bytes.freeze()).unwrap();
            // One packet in flight makes the test ordered, not the transport.
            let echoed = client.read_datagram().await.unwrap();
            let (parsed, payload) = DatagramHeader::parse(&echoed).unwrap();
            assert_eq!(parsed, header);
            assert_eq!(payload, b"opaque opus with redundancy");
        }
    };
    let echo = async {
        let request = server_control.recv().await.unwrap();
        assert_eq!(request.msg, Some(ping(17)));
        server_control
            .send(&ServerEnvelope {
                request_id: request.request_id,
                msg: Some(server_envelope::Msg::Pong(v1::Pong {
                    sent_at: 17,
                    server_time: 18,
                })),
            })
            .await
            .unwrap();
        for seq in 0..1000_u16 {
            let packet = peer.read_datagram().await.unwrap();
            assert_eq!(DatagramHeader::parse(&packet).unwrap().0.seq, seq);
            peer.send_datagram(packet).unwrap();
        }
    };
    within(async {
        tokio::join!(exchange, echo);
    })
    .await;
}

#[tokio::test]
async fn changed_certificate_is_distinct_and_never_replaces_pin() {
    let server = Server::new().await;
    let pins = Arc::new(MemoryPinStore::default());
    let key = ServerId::new("127.0.0.1", server.addr().port()).unwrap();
    let old = tls::fingerprint(b"previous certificate");
    pins.set(&key, old).unwrap();
    let result = within(client::connect(
        "127.0.0.1",
        server.addr().port(),
        pins.clone(),
    ))
    .await;
    assert!(
        matches!(result, Err(Error::FingerprintChanged { expected, received }) if expected == old && received == server.fingerprint)
    );
    assert_eq!(pins.get(&key).unwrap(), Some(old));
}

#[tokio::test]
async fn webpki_valid_certificate_is_recorded_but_cannot_override_pin() {
    let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).unwrap();
    let cert = generated.cert.der().clone();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    let server = Server::with_tls(TlsConfig {
        certificates: vec![cert],
        private_key: rustls::pki_types::PrivatePkcs8KeyDer::from(
            generated.signing_key.serialize_der(),
        )
        .into(),
    });
    let pins = Arc::new(MemoryPinStore::default());
    let key = ServerId::new("127.0.0.1", server.addr().port()).unwrap();
    let connection = within(client::connect_with_roots(
        "127.0.0.1",
        server.addr().port(),
        pins.clone(),
        roots.clone(),
    ))
    .await
    .unwrap();
    assert_eq!(pins.get(&key).unwrap(), Some(server.fingerprint));
    connection.close(0, b"done");
    pins.set(&key, tls::fingerprint(b"old")).unwrap();
    assert!(matches!(
        within(client::connect_with_roots(
            "127.0.0.1",
            server.addr().port(),
            pins,
            roots
        ))
        .await,
        Err(Error::FingerprintChanged { .. })
    ));
}

#[tokio::test]
async fn request_router_correlates_out_of_order_replies_and_events() {
    let mut server = Server::new().await;
    let (_client, stream, _peer, mut peer_stream) = server.pair().await;
    let (router, mut events) = ClientControl::start(stream);
    let requests = async {
        let (first, second) = tokio::join!(router.call(ping(11)), router.call(ping(22)));
        assert!(matches!(first.unwrap(), Reply::Ack(_)));
        assert!(matches!(second.unwrap(), Reply::Error(error) if error.message == "denied"));
        assert_eq!(events.recv().await.unwrap(), notice("event"));
    };
    let responses = async {
        let first = peer_stream.recv().await.unwrap();
        let second = peer_stream.recv().await.unwrap();
        assert_eq!(first.request_id, 2);
        assert_eq!(second.request_id, first.request_id + 1);
        peer_stream.send(&notice("event")).await.unwrap();
        peer_stream
            .send(&ServerEnvelope::error(
                second.request_id,
                v1::ErrorCode::Forbidden,
                "denied",
            ))
            .await
            .unwrap();
        peer_stream.send(&ack(first.request_id)).await.unwrap();
    };
    within(async {
        tokio::join!(requests, responses);
    })
    .await;
}

#[tokio::test]
async fn partial_reads_resume_after_cancellation_and_oversize_prefix_closes() {
    let mut server = Server::new().await;
    let mut client = within(client::connect(
        "127.0.0.1",
        server.addr().port(),
        server.pins(),
    ))
    .await
    .unwrap();
    let mut stream = client.open_control().await.unwrap();
    stream
        .send(&ClientEnvelope {
            request_id: 1,
            msg: Some(ping(0)),
        })
        .await
        .unwrap();
    let peer = server.next().await;
    let (mut raw_send, mut raw_recv) = within(peer.quic().accept_bi()).await.unwrap();
    let _: ClientEnvelope = frame::read_frame(&mut raw_recv).await.unwrap();
    let bytes = frame::encode(&notice("resumed")).unwrap();
    raw_send.write_all(&bytes[..2]).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), stream.recv())
            .await
            .is_err()
    );
    raw_send.write_all(&bytes[2..5]).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), stream.recv())
            .await
            .is_err()
    );
    raw_send.write_all(&bytes[5..]).await.unwrap();
    assert_eq!(within(stream.recv()).await.unwrap(), notice("resumed"));
    raw_send.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    assert!(
        matches!(within(stream.recv()).await, Err(ControlError::Frame(error)) if matches!(*error, frame::FrameError::TooLarge { .. }))
    );
    assert!(client.quic().close_reason().is_some());
}

#[tokio::test]
async fn pending_calls_fail_on_bad_response_ids_and_disconnect() {
    for mismatch in [true, false] {
        let mut server = Server::new().await;
        let (_client, stream, peer, mut peer_stream) = server.pair().await;
        let (router, _events) = ClientControl::start(stream);
        let call = async {
            assert!(router.call(ping(0)).await.is_err());
        };
        let failure = async {
            let request = peer_stream.recv().await.unwrap();
            if mismatch {
                let mut response = ack(request.request_id);
                response.request_id += 1;
                peer_stream.send(&response).await.unwrap();
            } else {
                peer.close(1, b"server stopped");
            }
        };
        within(async {
            tokio::join!(call, failure);
        })
        .await;
    }
}

#[tokio::test]
async fn one_control_stream_and_future_video_streams() {
    let mut server = Server::new().await;
    let (mut client, _control, mut peer, _peer_control) = server.pair().await;
    assert!(matches!(
        client.open_control().await,
        Err(Error::ControlAlreadyTaken)
    ));
    assert!(matches!(
        peer.accept_control().await,
        Err(Error::ControlAlreadyTaken)
    ));
    let video = async {
        let mut stream = client.quic().open_uni().await.unwrap();
        stream.write_all(b"future GOP").await.unwrap();
        stream.finish().unwrap();
    };
    let receive = async {
        let mut stream = peer.quic().accept_uni().await.unwrap();
        assert_eq!(stream.read_to_end(32).await.unwrap(), b"future GOP");
    };
    within(async {
        tokio::join!(video, receive);
    })
    .await;
}

#[tokio::test]
async fn cancelled_requests_do_not_reuse_ids_or_steal_later_replies() {
    let mut server = Server::new().await;
    let (_client, stream, _peer, mut peer_stream) = server.pair().await;
    let (router, _events) = ClientControl::start(stream);
    let cancelled_id = {
        let request = router.call(ping(10));
        tokio::pin!(request);
        tokio::select! {
            result = &mut request => panic!("request should be waiting: {result:?}"),
            request = peer_stream.recv() => request.unwrap().request_id,
        }
    };
    let next = async {
        assert!(matches!(
            router.call(ping(20)).await.unwrap(),
            Reply::Ack(_)
        ));
    };
    let replies = async {
        let request = peer_stream.recv().await.unwrap();
        assert_eq!(request.request_id, cancelled_id + 1);
        peer_stream.send(&ack(cancelled_id)).await.unwrap();
        peer_stream.send(&ack(request.request_id)).await.unwrap();
    };
    within(async {
        tokio::join!(next, replies);
    })
    .await;
}

#[tokio::test]
async fn event_overflow_closes_instead_of_silently_losing_state() {
    let mut server = Server::new().await;
    let (_client, stream, peer, mut peer_stream) = server.pair().await;
    let (router, _events) = ClientControl::start(stream);
    for _ in 0..257 {
        if peer_stream.send(&notice("queued")).await.is_err() {
            break;
        }
    }
    within(peer.quic().closed()).await;
    assert!(matches!(
        router.terminal_error(),
        Some(ControlError::Capacity)
    ));
}

#[tokio::test]
async fn idle_client_sends_protocol_ping() {
    let mut server = Server::new().await;
    let (_client, stream, _peer, mut peer_stream) = server.pair().await;
    let (_router, _events) = ClientControl::start(stream);
    let request = tokio::time::timeout(Duration::from_secs(17), peer_stream.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.request_id, 2);
    assert!(matches!(request.msg, Some(client_envelope::Msg::Ping(_))));
}

#[tokio::test]
async fn incompatible_alpn_and_missing_datagram_support_are_rejected() {
    for datagrams in [true, false] {
        let server = Server::new().await;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(server.certificate.clone()).unwrap();
        let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        tls.alpn_protocols = vec![if datagrams {
            b"wrong-protocol".to_vec()
        } else {
            ss_net::ALPN.to_vec()
        }];
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
        let mut config = quinn::ClientConfig::new(Arc::new(crypto));
        let mut transport = quinn::TransportConfig::default();
        if !datagrams {
            transport.datagram_receive_buffer_size(None);
        }
        config.transport_config(Arc::new(transport));
        let mut endpoint = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        endpoint.set_default_client_config(config);
        let result = within(endpoint.connect(server.addr(), "localhost").unwrap()).await;
        if datagrams {
            assert!(result.is_err(), "ALPN mismatch must reject the handshake");
        } else {
            let error = match result {
                Ok(connection) => within(connection.closed()).await,
                Err(error) => error,
            };
            assert!(
                matches!(error, quinn::ConnectionError::ApplicationClosed(reason) if reason.error_code == 1_u32.into())
            );
        }
    }
}

#[tokio::test]
async fn fresh_router_waits_for_hello_before_keepalive() {
    let mut server = Server::new().await;
    let mut client = within(client::connect(
        "127.0.0.1",
        server.addr().port(),
        server.pins(),
    ))
    .await
    .unwrap();
    let stream = client.open_control().await.unwrap();
    let peer = server.next().await;
    let (router, _events) = ClientControl::start(stream);
    assert!(
        tokio::time::timeout(Duration::from_secs(16), peer.quic().accept_bi())
            .await
            .is_err(),
        "router sent a frame before the application supplied Hello"
    );
    let hello = async {
        let response = router
            .request(client_envelope::Msg::Hello(v1::Hello {
                protocol_version: 1,
                ..Default::default()
            }))
            .await
            .unwrap();
        assert!(matches!(
            response.msg,
            Some(server_envelope::Msg::Welcome(_))
        ));
    };
    let receive = async {
        let (mut send, mut recv) = peer.quic().accept_bi().await.unwrap();
        let first: ClientEnvelope = frame::read_frame(&mut recv).await.unwrap();
        assert_eq!(first.request_id, 1);
        assert!(matches!(first.msg, Some(client_envelope::Msg::Hello(_))));
        frame::write_frame(
            &mut send,
            &ServerEnvelope {
                request_id: 1,
                msg: Some(server_envelope::Msg::Welcome(v1::Welcome::default())),
            },
        )
        .await
        .unwrap();
    };
    within(async {
        tokio::join!(hello, receive);
    })
    .await;
}

#[tokio::test]
async fn mtu_discovery_respects_a_smaller_peers_receive_limit() {
    let mut server = Server::new().await;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(server.certificate.clone()).unwrap();
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![ss_net::ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    // The stock Quinn endpoint advertises only 1472 bytes, like an older peer.
    let mut endpoint = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(crypto)));
    let client = within(endpoint.connect(server.addr(), "localhost").unwrap())
        .await
        .unwrap();
    let peer = server.next().await;
    let payload = bytes::Bytes::from_static(&[0x55; 1150]);
    within(async {
        for _ in 0..100 {
            peer.send_datagram(payload.clone()).unwrap();
            assert_eq!(client.read_datagram().await.unwrap(), payload);
            client.send_datagram(payload.clone()).unwrap();
            assert_eq!(peer.read_datagram().await.unwrap(), payload);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    assert!(peer.quic().stats().path.sent_plpmtud_probes > 0);
    assert!(peer.quic().stats().path.current_mtu <= 1472);
    assert!(peer.max_datagram_size().unwrap() < 1472);
    client.close(0_u32.into(), b"done");
}

#[tokio::test]
async fn ipv4_mapped_ipv6_client_reaches_an_ipv4_server() {
    let mut server = Server::new().await;
    let host = "::ffff:127.0.0.1";
    let pins = server.pins();
    pins.set(
        &ServerId::new(host, server.addr().port()).unwrap(),
        server.fingerprint,
    )
    .unwrap();
    let client = within(client::connect(host, server.addr().port(), pins))
        .await
        .unwrap();
    let peer = server.next().await;
    client
        .send_datagram(bytes::Bytes::from_static(b"dual stack"))
        .unwrap();
    assert_eq!(
        within(peer.read_datagram()).await.unwrap().as_ref(),
        b"dual stack"
    );
    client.close(0, b"done");
}
