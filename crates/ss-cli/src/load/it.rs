//! Load driver against the in-process authenticated server. No sound device.

use std::{sync::Arc, time::Duration};

use ss_core::VoiceProfile;
use ss_net::{
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};

use super::{Behavior, LoadOptions, Target, run};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bots_talk_post_churn_and_survive_a_reconnect_storm() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(
        ss_server::db::Pool::open(dir.path().join("socialspeak.db"))
            .await
            .unwrap(),
    );
    let owner = ss_server::auth::ensure_owner_invite(&pool)
        .await
        .unwrap()
        .unwrap();
    let identity = tls::generate_self_signed(dir.path()).await.unwrap();
    let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
    let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
    let port = endpoint.local_addr().unwrap().port();
    let pins = Arc::new(MemoryPinStore::default());
    pins.set(&ServerId::new("127.0.0.1", port).unwrap(), fingerprint)
        .unwrap();
    let server = Arc::new(
        ss_server::connection::Server::open(Arc::clone(&pool))
            .await
            .unwrap(),
    );
    let task = tokio::spawn(Arc::clone(&server).run(endpoint.clone()));
    let state = dir.path().join("state.txt");
    let csv = dir.path().join("run.csv");
    let options = LoadOptions {
        target: Target {
            host: "127.0.0.1".to_owned(),
            port,
            pins,
        },
        bots: 3,
        channels: 2,
        behavior: Behavior {
            duty: 1.0,
            burst: Duration::from_secs(1),
            text_per_minute: 30.0,
            churn: Some(Duration::from_millis(800)),
            away: Duration::from_millis(200),
        },
        storm_every: Some(Duration::from_millis(1_500)),
        duration: Duration::from_secs(4),
        interval: Duration::from_secs(1),
        ramp: Duration::from_millis(10),
        state: state.clone(),
        invite: Some(owner),
        prefix: "it".to_owned(),
        profile: VoiceProfile::Voice,
        csv: Some(csv.clone()),
        print: false,
    };

    let summary = run(options.clone()).await.unwrap();
    assert_eq!(summary.forced, 0, "{summary}");
    assert!(summary.storms >= 2, "{summary}");
    assert!(summary.storm_rejoins >= 3, "{summary}");
    assert!(summary.sessions >= 3 + summary.storm_rejoins, "{summary}");
    assert!(summary.sent > 50, "{summary}");
    assert!(summary.received > 0, "{summary}");
    assert!(summary.relay_p50.is_some(), "{summary}");
    assert!(summary.text_acked > 0, "{summary}");
    assert!(summary.lost * 20 < summary.received, "{summary}");
    assert!(summary.churn_leaves > 0, "{summary}");
    let media = server.media_stats();
    assert!(media.forwarded > 0);
    assert_eq!(media.dropped, 0);
    let rows = std::fs::read_to_string(&csv).unwrap();
    assert!(rows.lines().count() >= 4, "{rows}");

    // The second run reuses the stored tokens: no registration, no invite.
    let saved = std::fs::read_to_string(&state).unwrap();
    assert_eq!(
        saved
            .lines()
            .filter(|line| line.starts_with("user "))
            .count(),
        3
    );
    assert!(saved.lines().any(|line| line.starts_with("invite ")));
    let again = run(LoadOptions {
        invite: None,
        storm_every: None,
        duration: Duration::from_secs(1),
        csv: None,
        ..options
    })
    .await
    .unwrap();
    assert_eq!(again.forced, 0, "{again}");
    assert_eq!(again.sessions, 3, "{again}");
    assert_eq!(std::fs::read_to_string(&state).unwrap(), saved);

    endpoint.close(0, b"test ended");
    task.abort();
}
