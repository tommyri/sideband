use super::*;
use ss_client_core::{Credentials, ServerAddr, State as ClientState};
use ss_net::{pins::PinStore, server::Endpoint, tls};
use std::{sync::Arc, time::Duration};

fn input(name: &str) -> ChannelFields {
    ChannelFields {
        name: name.into(),
        topic: "Topic".into(),
        profile: 2,
        bitrate: Some(128_000),
        user_limit: Some(8),
    }
}
#[test]
fn validates_voice_ranges_and_keeps_clearing_semantics() {
    assert!(fields(input("Music"), 3).is_ok());
    let mut bad = input("Voice");
    bad.profile = 1;
    assert!(fields(bad, 3).is_err());
    let mut empty = input("Name");
    empty.topic.clear();
    empty.bitrate = None;
    empty.user_limit = None;
    let (_, topic, voice) = fields(empty, 3).unwrap();
    assert_eq!(topic, "");
    let voice = voice.unwrap();
    assert_eq!(voice.bitrate, None);
    assert_eq!(voice.user_limit, None);
    assert!(fields(input(""), 2).is_err());
    assert!(fields(input(&"x".repeat(65)), 2).is_err());
    assert!(fields(input("Text"), 2).unwrap().2.is_none());
    let mut bad = input("Limit");
    bad.user_limit = Some(100);
    assert!(fields(bad, 3).is_err());
}
struct World {
    owner: AppState,
    guest: AppState,
    key: String,
    task: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}
impl World {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            ss_server::db::Pool::open(dir.path().join("server.db"))
                .await
                .unwrap(),
        );
        let invite = ss_server::auth::ensure_owner_invite(&pool)
            .await
            .unwrap()
            .unwrap();
        let identity = tls::generate_self_signed(dir.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let address = ServerAddr::new("127.0.0.1", endpoint.local_addr().unwrap().port()).unwrap();
        let key = address.id().unwrap();
        let server = Arc::new(ss_server::connection::Server::open(pool).await.unwrap());
        let task = tokio::spawn(server.run(endpoint));
        let owner = AppState::open(&dir.path().join("owner")).unwrap();
        owner.pins.set(&key, fingerprint).unwrap();
        let client = Client::connect_with_storage(
            address.clone(),
            Credentials::Register(v1::Register {
                invite_code: invite,
                username: "owner".into(),
                password: "test-password".into(),
            }),
            owner.pins.clone(),
            owner.storage.clone(),
        )
        .await
        .unwrap();
        let invite = client
            .create_invite(v1::CreateInvite::default())
            .await
            .unwrap();
        owner.clients.lock().await.insert(client).unwrap();
        let guest = AppState::open(&dir.path().join("guest")).unwrap();
        guest.pins.set(&key, fingerprint).unwrap();
        let client = Client::connect_with_storage(
            address,
            Credentials::Register(v1::Register {
                invite_code: invite.code,
                username: "guest".into(),
                password: "test-password".into(),
            }),
            guest.pins.clone(),
            guest.storage.clone(),
        )
        .await
        .unwrap();
        guest.clients.lock().await.insert(client).unwrap();
        Self {
            owner,
            guest,
            key: key.as_str().to_owned(),
            task,
            _dir: dir,
        }
    }
    async fn create(&self, name: &str, kind: i32, parent: Option<String>) -> String {
        perform(
            &self.owner,
            &self.key,
            ChannelAdminAction::Create {
                channel_kind: kind,
                parent_id: parent,
                fields: input(name),
            },
        )
        .await
        .unwrap();
        wait(&self.owner, &self.key, |s| {
            s.channels.values().any(|c| c.name == name)
        })
        .await;
        let clients = self.owner.clients.lock().await;
        let store = clients
            .get(&parse_server_id(&self.key).unwrap())
            .unwrap()
            .state();
        let snapshot = store.read();
        snapshot
            .channels
            .iter()
            .find(|(_, c)| c.name == name)
            .unwrap()
            .0
            .to_string()
    }
}
async fn wait(state: &AppState, server: &str, predicate: impl Fn(&ClientState) -> bool) {
    let clients = state.clients.lock().await;
    let client = clients.get(&parse_server_id(server).unwrap()).unwrap();
    let mut events = client.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if predicate(&client.state().read()) {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn real_bridge_creates_edits_moves_and_cascade_deletes() {
    let world = World::new().await;
    let denied = perform(
        &world.guest,
        &world.key,
        ChannelAdminAction::Create {
            channel_kind: 2,
            parent_id: None,
            fields: input("denied"),
        },
    )
    .await;
    assert!(denied.is_err());
    let category = world.create("Testing", 1, None).await;
    let voice = world.create("Music", 3, Some(category.clone())).await;
    let child = world.create("chat", 2, Some(category.clone())).await;
    let voice_id = parse(&voice).unwrap();
    wait(&world.guest, &world.key, |s| {
        s.channels.contains_key(&voice_id)
    })
    .await;
    let mut edited = input("Stage");
    edited.topic.clear();
    edited.bitrate = None;
    edited.user_limit = None;
    perform(
        &world.owner,
        &world.key,
        ChannelAdminAction::Edit {
            channel: voice.clone(),
            fields: edited,
        },
    )
    .await
    .unwrap();
    wait(&world.guest, &world.key, |s| {
        s.channels.get(&voice_id).is_some_and(|c| {
            c.name == "Stage"
                && c.topic.is_none()
                && c.voice
                    .as_ref()
                    .is_some_and(|v| v.bitrate.is_none() && v.user_limit.is_none())
        })
    })
    .await;
    perform(
        &world.owner,
        &world.key,
        ChannelAdminAction::Move {
            channel: voice.clone(),
            parent_id: None,
            position: 0,
        },
    )
    .await
    .unwrap();
    wait(&world.owner, &world.key, |s| {
        s.channels
            .get(&voice_id)
            .is_some_and(|c| c.parent_id.is_none() && c.position == 0)
    })
    .await;
    wait(&world.guest, &world.key, |s| {
        s.channels
            .get(&voice_id)
            .is_some_and(|c| c.parent_id.is_none() && c.position == 0)
            && s.channels
                .values()
                .filter(|c| c.parent_id.is_none() && c.position == 0)
                .count()
                == 1
    })
    .await;
    // The server independently rejects a stale/malicious client even without the native guard.
    {
        let clients = world.guest.clients.lock().await;
        let client = clients.get(&parse_server_id(&world.key).unwrap()).unwrap();
        assert!(
            client
                .delete_channel(v1::DeleteChannel {
                    channel_id: wire(&voice).unwrap()
                })
                .await
                .is_err()
        );
    }
    assert!(
        perform(
            &world.owner,
            &world.key,
            ChannelAdminAction::Move {
                channel: category.clone(),
                parent_id: Some(category.clone()),
                position: 0
            }
        )
        .await
        .is_err()
    );
    perform(
        &world.owner,
        &world.key,
        ChannelAdminAction::Delete {
            channel: category.clone(),
        },
    )
    .await
    .unwrap();
    let category_id = parse(&category).unwrap();
    let child_id = parse(&child).unwrap();
    wait(&world.guest, &world.key, |s| {
        !s.channels.contains_key(&category_id)
            && !s.channels.contains_key(&child_id)
            && s.channels.contains_key(&voice_id)
    })
    .await;
    perform(
        &world.owner,
        &world.key,
        ChannelAdminAction::Delete { channel: voice },
    )
    .await
    .unwrap();
    wait(&world.guest, &world.key, |s| {
        !s.channels.contains_key(&voice_id)
    })
    .await;
    world.task.abort();
}
