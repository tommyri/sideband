use super::*;
use ss_client_core::{Client, Credentials, ServerAddr, State as ClientState};
use ss_net::{pins::PinStore, server::Endpoint, tls};
use std::{sync::Arc, time::Duration};
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

fn input(name: &str) -> RoleFields {
    RoleFields {
        name: name.into(),
        color: Some(0x12_34_56),
        permissions: Permissions::SEND_MESSAGES.bits().to_string(),
    }
}
#[tokio::test]
async fn real_bridge_roles_assignments_reorder_clear_and_delete() {
    let world = World::new().await;
    assert!(
        perform(
            &world.guest,
            &world.key,
            RoleAction::Create {
                fields: input("Denied")
            }
        )
        .await
        .is_err()
    );
    perform(
        &world.owner,
        &world.key,
        RoleAction::Create {
            fields: input("Helper"),
        },
    )
    .await
    .unwrap();
    wait(&world.guest, &world.key, |s| {
        s.roles.values().any(|r| r.name == "Helper")
    })
    .await;
    let (role, guest) = {
        let clients = world.guest.clients.lock().await;
        let store = clients
            .get(&parse_server_id(&world.key).unwrap())
            .unwrap()
            .state();
        let state = store.read();
        (
            display(
                &state
                    .roles
                    .values()
                    .find(|r| r.name == "Helper")
                    .unwrap()
                    .id,
            )
            .unwrap(),
            display(&state.own_user.as_ref().unwrap().id).unwrap(),
        )
    };
    perform(
        &world.owner,
        &world.key,
        RoleAction::Assign {
            member: guest.clone(),
            roles: vec![role.clone()],
        },
    )
    .await
    .unwrap();
    wait(&world.guest, &world.key, |s| {
        s.members
            .get(&parse(&guest).unwrap())
            .unwrap()
            .roles
            .contains(&wire(&role).unwrap())
    })
    .await;
    let mut edited = input("Guide");
    edited.color = None;
    perform(
        &world.owner,
        &world.key,
        RoleAction::Edit {
            role: role.clone(),
            fields: edited,
        },
    )
    .await
    .unwrap();
    wait(&world.guest, &world.key, |s| {
        s.roles
            .get(&parse(&role).unwrap())
            .is_some_and(|r| r.name == "Guide" && r.color.is_none())
    })
    .await;
    perform(
        &world.owner,
        &world.key,
        RoleAction::Move {
            role: role.clone(),
            before: None,
        },
    )
    .await
    .unwrap();
    wait(&world.guest, &world.key, |s| {
        s.roles
            .get(&parse(&role).unwrap())
            .is_some_and(|r| r.position == 0)
            && s.roles.values().filter(|r| r.position == 0).count() == 1
    })
    .await;
    perform(
        &world.owner,
        &world.key,
        RoleAction::Delete { role: role.clone() },
    )
    .await
    .unwrap();
    wait(&world.guest, &world.key, |s| {
        !s.roles.contains_key(&parse(&role).unwrap())
            && !s
                .members
                .get(&parse(&guest).unwrap())
                .unwrap()
                .roles
                .contains(&wire(&role).unwrap())
    })
    .await;
    world.task.abort();
}

#[test]
fn snapshot_matches_server_hierarchy_and_administrator_bypass() {
    let own = Id::generate();
    let peer = Id::generate();
    let junior = Id::generate();
    let everyone = Id::generate();
    let manager = Id::generate();
    let helper = Id::generate();
    let admin = Id::generate();
    let user = |id: Id| v1::User {
        id: id.as_bytes().to_vec().into(),
        username: id.to_string(),
        ..Default::default()
    };
    let mut state = ClientState {
        connection: ConnectionState::Connected,
        own_user: Some(user(own)),
        ..Default::default()
    };
    for (id, name, position, permissions, is_default) in [
        (everyone, "Everyone", 0, Permissions::EVERYONE.bits(), true),
        (
            manager,
            "Manager",
            10,
            Permissions::MANAGE_ROLES.bits(),
            false,
        ),
        (helper, "Helper", 1, 0, false),
        (admin, "Admin", 2, 1, false),
    ] {
        state.roles.insert(
            id,
            v1::Role {
                id: id.as_bytes().to_vec().into(),
                name: name.into(),
                position,
                permissions,
                is_default,
                color: None,
            },
        );
    }
    for (id, roles) in [
        (own, vec![manager]),
        (peer, vec![manager]),
        (junior, vec![helper]),
    ] {
        state.members.insert(
            id,
            v1::Member {
                user: Some(user(id)),
                roles: roles
                    .into_iter()
                    .map(|id| id.as_bytes().to_vec().into())
                    .collect(),
                ..Default::default()
            },
        );
    }
    let projected = snapshot(&state).unwrap();
    assert!(projected.manage_roles);
    assert!(!projected.manage_channels);
    assert!(!projected.administrator);
    assert!(
        !projected
            .roles
            .iter()
            .find(|r| r.id == admin.to_string())
            .unwrap()
            .editable
    );
    assert!(
        !projected
            .members
            .iter()
            .find(|m| m.id == peer.to_string())
            .unwrap()
            .editable
    );
    assert!(
        !projected
            .members
            .iter()
            .find(|m| m.id == own.to_string())
            .unwrap()
            .editable
    );
    assert!(
        projected
            .members
            .iter()
            .find(|m| m.id == junior.to_string())
            .unwrap()
            .editable
    );
    state.members.get_mut(&own).unwrap().roles = vec![admin.as_bytes().to_vec().into()];
    let projected = snapshot(&state).unwrap();
    assert!(projected.administrator && projected.manage_channels);
    assert!(projected.roles.iter().all(|r| r.editable));
    assert!(projected.members.iter().all(|m| m.editable));
    state.connection = ConnectionState::Disconnected;
    assert!(snapshot(&state).is_err());
}
