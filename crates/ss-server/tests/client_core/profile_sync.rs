use super::{Client, Credentials, World, sign_in_profile, v1};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use ss_client_core::{profile::ProfileCredentials, profile_sync::RelayKey, storage::Storage};
use ss_server::{
    config::Config,
    http::{hardening, info},
    tls,
};
use tokio::task::JoinHandle;
use tower::ServiceExt;

async fn start_https(world: &mut World) -> JoinHandle<()> {
    let identity = tls::prepare(&Config::default(), world.dir.path())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    world.address.https_port = listener.local_addr().unwrap().port();
    let router = world.server.public_routes().merge(info::router(
        world.pool.clone(),
        identity.fingerprint_slot(),
    ));
    tokio::spawn(hardening::serve_tls(
        listener,
        router,
        identity.https_config(),
        std::future::pending(),
    ))
}

async fn advertised(world: &World) -> bool {
    use ss_net::pins::PinStore;
    let trust = world
        .pins
        .policy(&world.address.id().unwrap())
        .unwrap()
        .unwrap();
    let http = reqwest::Client::builder()
        .use_preconfigured_tls(ss_net::client::tls_config(trust).unwrap())
        .no_proxy()
        .build()
        .unwrap();
    let json = http
        .get(format!(
            "https://127.0.0.1:{}/api/v1/server",
            world.address.https_port
        ))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let meta: serde_json::Value = serde_json::from_slice(&json).unwrap();
    meta["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "profileSync")
}

async fn incomplete_upload_keeps_existing_copy(
    world: &World,
    owner: &Client,
    key: &RelayKey,
    copy: &[u8],
) {
    let id = world.address.id().unwrap();
    let token = world.storage.session(&id).unwrap().unwrap();
    let app = world.server.public_routes();
    let route = "/api/v1/profile-sync/copy";
    let unauth = app
        .clone()
        .oneshot(Request::get(route).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
    let over = app
        .clone()
        .oneshot(
            Request::put(route)
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::from(vec![1; 512 * 1024 + 1]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(over.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let interrupted = Body::from_stream(futures::stream::iter([
        Ok(axum::body::Bytes::from_static(&[1; 64])),
        Err(std::io::Error::other("client disconnected")),
    ]));
    let partial = app
        .oneshot(
            Request::put(route)
                .header("Authorization", format!("Bearer {token}"))
                .body(interrupted)
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!partial.status().is_success());
    assert_eq!(
        owner.relay_copy(key).await.unwrap().unwrap().ciphertext,
        copy
    );
}

#[tokio::test]
async fn encrypted_copy_is_member_private_atomic_and_survives_restart() {
    let mut world = World::new().await;
    let http = start_https(&mut world).await;
    let id = world.address.id().unwrap();
    let key = RelayKey::new(&[1; 32], id.clone()).unwrap();
    let owner = sign_in_profile(&world, 2, &world.invite).await.unwrap();
    let mut fields = owner.server_settings().await.unwrap().settings;
    assert_eq!(fields.profile_sync, Some(false));
    assert!(!advertised(&world).await);
    assert!(!owner.relay_eligible(&key).await.unwrap());
    fields.profile_sync = Some(true);
    let saved = owner.save_server_settings(&fields).await.unwrap();
    assert_eq!(saved.settings.public_icon, Some(false));
    assert!(advertised(&world).await);
    assert!(owner.relay_eligible(&key).await.unwrap());
    let copy = key.seal(b"member's private community").unwrap();
    owner
        .write_relay_copy(&key, None, copy.clone())
        .await
        .unwrap();
    // Simulate losing the acknowledgement. The retry succeeds without replacing the copy.
    owner
        .write_relay_copy(&key, None, copy.clone())
        .await
        .unwrap();
    assert!(
        owner
            .write_relay_copy(&key, None, key.seal(b"stale").unwrap())
            .await
            .is_err()
    );
    assert_eq!(
        owner.relay_copy(&key).await.unwrap().unwrap().ciphertext,
        copy
    );
    let outsider = world.guest(&owner).await;
    assert!(!outsider.relay_eligible(&key).await.unwrap());
    assert!(outsider.relay_copy(&key).await.is_err());
    let invite = owner
        .create_invite(v1::CreateInvite::default())
        .await
        .unwrap();
    let other = Client::connect(
        world.address.clone(),
        Credentials::Profile(Box::new(
            ProfileCredentials::new(&[9; 32], &[8; 32], &id, "second_member".into(), invite.code)
                .unwrap(),
        )),
        world.pins.clone(),
    )
    .await
    .unwrap();
    let other_key = RelayKey::new(&[9; 32], id.clone()).unwrap();
    assert!(other.relay_eligible(&other_key).await.unwrap());
    assert!(!other.relay_eligible(&key).await.unwrap());
    assert!(other.relay_copy(&other_key).await.unwrap().is_none());
    assert!(
        other
            .write_relay_copy(&key, None, copy.clone())
            .await
            .is_err()
    );
    // Upgraded operator UI cannot be used by members to enable/disable hosting.
    assert!(other.save_server_settings(&saved.settings).await.is_err());
    incomplete_upload_keeps_existing_copy(&world, &owner, &key, &copy).await;
    owner.close().await;
    outsider.close().await;
    other.close().await;
    http.abort();
    world.restart().await;
    let http = start_https(&mut world).await;
    let owner = sign_in_profile(&world, 2, "").await.unwrap();
    let mut stored = owner.relay_copy(&key).await.unwrap().unwrap();
    assert_eq!(
        key.open(&mut stored.ciphertext).unwrap(),
        b"member's private community"
    );
    owner
        .write_relay_copy(&key, Some(&stored.etag), key.seal(b"new revision").unwrap())
        .await
        .unwrap();
    assert!(
        owner
            .write_relay_copy(&key, Some(&stored.etag), copy)
            .await
            .is_err()
    );
    let mut fields = owner.server_settings().await.unwrap().settings;
    fields.profile_sync = Some(false);
    owner.save_server_settings(&fields).await.unwrap();
    assert!(!advertised(&world).await);
    assert!(owner.relay_copy(&key).await.is_err());
    assert!(
        owner
            .write_relay_copy(&key, None, key.seal(b"forbidden").unwrap())
            .await
            .is_err()
    );
    http.abort();
}
