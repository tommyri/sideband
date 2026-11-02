//! Self-signed fingerprints, file reload, and the public server description.

use std::{
    net::SocketAddr,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
    routing::get,
};
use serde_json::Value;
use ss_net::{
    client,
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls::{CertResolver, Fingerprint},
};
use ss_server::{
    config::{Config, TlsMode},
    db, http, tls,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
use tower::ServiceExt;

#[tokio::test]
async fn self_signed_fingerprint_is_stable_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let first = ss_net::tls::generate_self_signed(dir.path()).await.unwrap();
    let second = ss_net::tls::generate_self_signed(dir.path()).await.unwrap();
    assert_eq!(hex(&first), hex(&second));
    assert_eq!(hex(&first).len(), 64);
}

#[test]
fn tls_rotate_replaces_the_self_signed_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let created = succeed(&["init", "--data-dir", path]);
    let first = line(&created, "fingerprint: ");
    let rotated = succeed(&["tls", "rotate", "--data-dir", path]);
    let second = line(&rotated, "fingerprint: ");
    assert_ne!(first, second);
    let printed = succeed(&["fingerprint", "--data-dir", path]);
    assert_eq!(line(&printed, "fingerprint: "), second);
}

#[tokio::test]
async fn files_mode_reloads_without_dropping_the_quic_socket() {
    let dir = tempfile::tempdir().unwrap();
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    let (cert, key) = issue("one.example");
    std::fs::write(&cert_path, &cert).unwrap();
    std::fs::write(&key_path, &key).unwrap();
    let mut config = Config::default();
    config.tls.mode = TlsMode::Files;
    config.tls.cert_path = cert_path.display().to_string();
    config.tls.key_path = key_path.display().to_string();
    let identity = tls::prepare(&config, dir.path()).await.unwrap();
    let first = identity.fingerprint().unwrap();
    let endpoint = Endpoint::bind_resolver(
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        identity.resolver(),
    )
    .unwrap();
    let port = endpoint.local_addr().unwrap().port();
    let accepting = endpoint.clone();
    tokio::spawn(async move {
        while let Some(incoming) = accepting.accept().await {
            tokio::spawn(async move {
                if let Ok(connection) = incoming.accept().await {
                    while connection.read_datagram().await.is_ok() {}
                }
            });
        }
    });
    let pins = Arc::new(MemoryPinStore::default());
    pins.set(&ServerId::new("127.0.0.1", port).unwrap(), first)
        .unwrap();
    let connection = timeout(
        Duration::from_secs(5),
        client::connect("127.0.0.1", port, Arc::clone(&pins) as _),
    )
    .await
    .unwrap()
    .unwrap();
    send(&connection).await;

    let (next_cert, next_key) = issue("two.example");
    publish(&cert_path, &next_cert);
    publish(&key_path, &next_key);
    let next = wait_for_fingerprint(&identity, first).await;
    assert_eq!(endpoint.local_addr().unwrap().port(), port);
    send(&connection).await;
    let Err(changed) = client::connect("127.0.0.1", port, pins).await else {
        panic!("expected the replacement certificate to be rejected");
    };
    match changed {
        ss_net::Error::FingerprintChanged { received, .. } => assert_eq!(received, next),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn files_mode_requires_the_certificate_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.tls.mode = TlsMode::Files;
    let Err(error) = tls::prepare(&config, dir.path()).await else {
        panic!("files mode accepted missing paths");
    };
    assert!(error.to_string().contains("cert_path"), "{error}");
}

#[tokio::test]
async fn public_listener_refuses_plain_http_and_accepts_pinned_https() {
    let dir = tempfile::tempdir().unwrap();
    let certificate = ss_net::tls::generate_self_signed(dir.path()).await.unwrap();
    let resolver = Arc::new(CertResolver::new(certificate).unwrap());
    let fingerprint = resolver.fingerprint();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(http::hardening::serve_tls(
        listener,
        Router::new().route("/private", get(private)),
        tls::https_config(resolver),
        async move {
            let _ = stop_rx.await;
        },
    ));

    let mut plain = tokio::net::TcpStream::connect(addr).await.unwrap();
    plain
        .write_all(b"GET /private HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer secret\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), plain.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&response).contains("200 OK"),
        "plain HTTP reached the authenticated route"
    );

    let response = pinned_client(fingerprint)
        .get(format!("https://{addr}/private"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let wrong_dir = tempfile::tempdir().unwrap();
    let wrong = ss_net::tls::generate_self_signed(wrong_dir.path())
        .await
        .unwrap();
    let wrong = ss_net::tls::fingerprint(wrong.certificates[0].as_ref());
    assert!(
        pinned_client(wrong)
            .get(format!("https://{addr}/private"))
            .bearer_auth("secret")
            .send()
            .await
            .is_err(),
        "an unapproved fingerprint reached the authenticated route"
    );

    let _ = stop_tx.send(());
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn public_https_uses_a_rotated_certificate_without_rebinding() {
    let dir = tempfile::tempdir().unwrap();
    let identity = tls::prepare(&Config::default(), dir.path()).await.unwrap();
    let first = identity.fingerprint().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(http::hardening::serve_tls(
        listener,
        Router::new().route("/private", get(private)),
        identity.https_config(),
        async move {
            let _ = stop_rx.await;
        },
    ));
    assert_eq!(
        pinned_client(first)
            .get(format!("https://{addr}/private"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    tls::rotate_self_signed(dir.path()).await.unwrap();
    let next = wait_for_fingerprint(&identity, first).await;
    assert!(
        pinned_client(first)
            .get(format!("https://{addr}/private"))
            .bearer_auth("secret")
            .send()
            .await
            .is_err(),
        "the old pin accepted the rotated certificate"
    );
    assert_eq!(
        pinned_client(next)
            .get(format!("https://{addr}/private"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let _ = stop_tx.send(());
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn acme_rejects_a_local_name_and_a_missing_email() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.tls.mode = TlsMode::Acme;
    config.server.public_host = "localhost".to_owned();
    config.tls.acme_email = "ops@example.com".to_owned();
    let Err(error) = tls::prepare(&config, dir.path()).await else {
        panic!("acme accepted localhost");
    };
    assert!(error.to_string().contains("public_host"), "{error}");

    config.server.public_host = "voice.example".to_owned();
    config.tls.acme_email.clear();
    let Err(error) = tls::prepare(&config, dir.path()).await else {
        panic!("acme accepted a missing email");
    };
    assert!(error.to_string().contains("acme_email"), "{error}");

    config.tls.acme_email = "ops@example.com".to_owned();
    config.server.bind_http.set_port(80);
    let Err(error) = tls::prepare(&config, dir.path()).await else {
        panic!("acme accepted the HTTPS listener on its HTTP-01 port");
    };
    assert!(error.to_string().contains("bind_http"), "{error}");
}

#[tokio::test]
async fn server_info_is_public() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(
        db::Pool::open(dir.path().join("socialspeak.db"))
            .await
            .unwrap(),
    );
    let fingerprint = Fingerprint::try_from("ab".repeat(32)).unwrap();
    let slot = Arc::new(arc_swap::ArcSwap::new(Arc::new(Some(fingerprint))));
    let response = http::info::router(pool, slot)
        .oneshot(
            Request::builder()
                .uri("/api/v1/server")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["name"], "SocialSpeak");
    assert_eq!(value["registrationMode"], "invite");
    assert_eq!(value["fingerprint"], "ab".repeat(32));
    assert!(value["version"].as_str().unwrap().contains('.'));
}

fn hex(tls: &ss_net::tls::TlsConfig) -> String {
    let leaf = tls.certificates.first().unwrap();
    ss_net::tls::fingerprint(leaf.as_ref()).to_string()
}

async fn private(headers: HeaderMap) -> StatusCode {
    if headers
        .get(header::AUTHORIZATION)
        .is_some_and(|value| value == "Bearer secret")
    {
        StatusCode::OK
    } else {
        StatusCode::UNAUTHORIZED
    }
}

fn pinned_client(fingerprint: Fingerprint) -> reqwest::Client {
    reqwest::Client::builder()
        .use_preconfigured_tls(client::pinned_tls_config(fingerprint).unwrap())
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn issue(name: &str) -> (String, String) {
    let issued = rcgen::generate_simple_self_signed(vec![name.to_owned()]).unwrap();
    (issued.cert.pem(), issued.signing_key.serialize_pem())
}

fn publish(path: &std::path::Path, contents: &str) {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, contents).unwrap();
    std::fs::rename(temporary, path).unwrap();
}

async fn send(connection: &ss_net::Connection) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if connection
            .send_datagram(bytes::Bytes::from_static(b"voice"))
            .is_ok()
        {
            return;
        }
        assert!(Instant::now() <= deadline, "datagram was not accepted");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_fingerprint(identity: &tls::Identity, previous: Fingerprint) -> Fingerprint {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(fingerprint) = identity.fingerprint()
            && fingerprint != previous
        {
            return fingerprint;
        }
        assert!(Instant::now() <= deadline, "certificate did not reload");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn succeed(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_ss-server"))
        .args(args)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?}\n{stderr}",
        output.status
    );
    String::from_utf8(output.stdout).unwrap()
}

fn line(output: &str, prefix: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap()
        .trim()
        .to_owned()
}
