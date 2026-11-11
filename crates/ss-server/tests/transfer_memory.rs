//! 200 stalled HTTP/2 downloads of one 25 MiB attachment stay within a fixed
//! resident-memory bound. This is its own test binary, so no other test runs
//! in the process whose RSS it samples.
#![cfg(unix)]

use std::{process::Command, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use ss_proto::v1;
use ss_server::{
    auth, db,
    http::{self, hardening},
    perms,
    storage::{Store, UploadPolicy},
};

const FILE_BYTES: usize = 25 * 1024 * 1024;
/// hyper's default `SETTINGS_MAX_CONCURRENT_STREAMS`, so one connection
/// carries every request.
const STREAMS: usize = 200;
/// Allowed RSS growth while every stream is stalled, in KiB. A server that
/// buffers each accepted download in full needs `25 MiB × accepted`; one that
/// streams needs one read chunk plus what flow control lets it queue.
const BOUND_KIB: u64 = 64 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_downloads_keep_resident_memory_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(
        db::Pool::open(dir.path().join("socialspeak.db"))
            .await
            .unwrap(),
    );
    let code = auth::ensure_owner_invite(&pool).await.unwrap().unwrap();
    let owner = auth::Auth::new(Arc::clone(&pool))
        .hello(&register(&code), "203.0.113.9".parse().unwrap())
        .await
        .unwrap();
    let perms = Arc::new(
        perms::PermissionService::open(Arc::clone(&pool))
            .await
            .unwrap(),
    );
    let default = UploadPolicy::default();
    let file = {
        let store = Store::open_in(Arc::clone(&pool), dir.path().to_path_buf(), default.clone())
            .await
            .unwrap();
        let bytes: Vec<u8> = (0..=u8::MAX).cycle().take(FILE_BYTES).collect();
        store
            .save(owner.user.id, "big.bin", &bytes)
            .await
            .unwrap()
            .id
    };

    for (policy, expect_ok) in [
        (default.clone(), default.transfers_per_account),
        (
            UploadPolicy {
                transfers_per_account: STREAMS,
                ..default.clone()
            },
            STREAMS,
        ),
    ] {
        let store = Store::open_in(Arc::clone(&pool), dir.path().to_path_buf(), policy)
            .await
            .unwrap();
        let app = hardening::harden(http::attachments::router(
            Arc::new(auth::Auth::new(Arc::clone(&pool))),
            Arc::clone(&perms),
            store,
        ));
        let (ok, refused, growth) = stall(app, file, &owner.resume_token).await;
        assert_eq!(ok, expect_ok, "accepted downloads");
        assert_eq!(
            ok + refused,
            STREAMS,
            "every other request is refused with 429"
        );
        assert!(
            growth < BOUND_KIB,
            "{ok} stalled downloads grew RSS by {growth} KiB (bound {BOUND_KIB} KiB)"
        );
    }
}

/// Opens one HTTP/2 connection, sends [`STREAMS`] downloads on it, never reads
/// a body, and returns `(200s, 429s, peak RSS growth in KiB)`.
async fn stall(app: axum::Router, file: ss_core::Id, token: &str) -> (usize, usize, u64) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(hardening::serve(listener, app, async move {
        let _ = stop_rx.await;
    }));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let baseline = rss_kib();

    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
            .await
            .unwrap();
    let driver = tokio::spawn(connection);
    let uri = format!("http://{addr}/api/v1/attachments/{file}/big.bin");
    let requests = (0..STREAMS).map(|_| {
        let mut sender = sender.clone();
        let request = Request::get(&uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        async move { sender.send_request(request).await.unwrap() }
    });
    let responses =
        tokio::time::timeout(Duration::from_secs(60), futures::future::join_all(requests))
            .await
            .expect("response heads did not arrive");
    let ok = responses
        .iter()
        .filter(|response| response.status() == StatusCode::OK)
        .count();
    let refused = responses
        .iter()
        .filter(|response| response.status() == StatusCode::TOO_MANY_REQUESTS)
        .count();

    // Let the server queue whatever flow control allows, then sample.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut peak = 0;
    for _ in 0..5 {
        peak = peak.max(rss_kib());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    drop(responses);
    drop(sender);
    driver.abort();
    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), served).await;
    (ok, refused, peak.saturating_sub(baseline))
}

fn rss_kib() -> u64 {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap()
}

fn register(code: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: "test".into(),
        auth: Some(v1::hello::Auth::Register(v1::Register {
            invite_code: code.into(),
            username: "ada".into(),
            password: "password-ok".into(),
        })),
        ..Default::default()
    }
}
