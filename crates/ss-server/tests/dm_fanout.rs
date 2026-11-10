//! Limits that keep one member from locking others out (T-091): group DM
//! caps, a bounded `Welcome`, scoped DM events, and fan-out that closes a
//! connection instead of dropping a frame.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use bytes::Bytes;
use sqlx::Connection;
use ss_core::{ChannelKind, Id, Timestamp, UserKind};
use ss_net::{
    client,
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};
use ss_proto::{
    v1::{self, client_envelope::Msg, server_envelope::Msg as Event},
    validate::{id_from_bytes, id_to_bytes},
};
use ss_server::{
    auth, connection,
    db::{self, NewInvite, NewMessage, NewUser, UserProfile, invites, messages, users},
    limits, registry,
};
use tokio::time::timeout;

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(10);

struct World {
    endpoint: Endpoint,
    port: u16,
    pins: Arc<MemoryPinStore>,
    task: tokio::task::JoinHandle<()>,
    server: Arc<connection::Server>,
    pool: Arc<db::Pool>,
    code: String,
    owner: Option<Id>,
    _dir: tempfile::TempDir,
}

impl World {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            db::Pool::open(dir.path().join("socialspeak.db"))
                .await
                .unwrap(),
        );
        let code = auth::ensure_owner_invite(&pool).await.unwrap().unwrap();
        let identity = tls::generate_self_signed(dir.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let port = endpoint.local_addr().unwrap().port();
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(&ServerId::new("127.0.0.1", port).unwrap(), fingerprint)
            .unwrap();
        let server = Arc::new(connection::Server::open(Arc::clone(&pool)).await.unwrap());
        let task = tokio::spawn(Arc::clone(&server).run(endpoint.clone()));
        Self {
            endpoint,
            port,
            pins,
            task,
            server,
            pool,
            code,
            owner: None,
            _dir: dir,
        }
    }

    async fn connect(&self) -> (ss_net::Connection, ss_net::control::ClientStream) {
        let mut connection = timeout(
            DEADLINE,
            client::connect("127.0.0.1", self.port, self.pins.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        let stream = connection.open_control().await.unwrap();
        (connection, stream)
    }

    /// Registers an account that can log in and returns its session token.
    /// The first one uses the owner code and becomes the administrator.
    async fn account(&mut self, username: &str) -> (Id, String) {
        let code = match self.owner {
            None => self.code.clone(),
            Some(owner) => {
                let mut conn = self.pool.write().await;
                invites::insert(
                    &mut conn,
                    NewInvite {
                        id: Id::generate(),
                        code: username.into(),
                        created_by: owner,
                        max_uses: Some(1),
                        expires_at: None,
                    },
                )
                .await
                .unwrap();
                username.into()
            }
        };
        let registered = auth::Auth::new(Arc::clone(&self.pool))
            .hello(
                &v1::Hello {
                    protocol_version: 1,
                    client_name: "test".into(),
                    auth: Some(v1::hello::Auth::Register(v1::Register {
                        invite_code: code,
                        username: username.into(),
                        password: PASSWORD.into(),
                    })),
                    ..Default::default()
                },
                "203.0.113.9".parse().unwrap(),
            )
            .await
            .unwrap();
        self.owner.get_or_insert(registered.user.id);
        (registered.user.id, registered.resume_token)
    }

    /// An account that never logs in. Cheaper than [`Self::account`].
    async fn bare(&self, username: &str) -> Id {
        let mut conn = self.pool.write().await;
        users::insert(
            &mut conn,
            NewUser {
                id: Id::generate(),
                username: username.into(),
                display_name: username.into(),
                password_hash: "unused".into(),
                kind: UserKind::User,
            },
        )
        .await
        .unwrap()
        .id
    }

    /// Writes `count` group conversations straight into the database, the way
    /// an attacker would have left them before any cap existed. Oldest first.
    async fn seed_groups(
        &self,
        count: usize,
        created_by: Id,
        members: &[Id],
        name: &str,
    ) -> Vec<Id> {
        let mut guard = self.pool.write().await;
        let mut tx = guard.begin().await.unwrap();
        let now = Timestamp::now().as_millis();
        let rows = vec!["(?, ?)"; members.len()].join(", ");
        let insert_members =
            format!("INSERT INTO channel_members (channel_id, user_id) VALUES {rows}");
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            let id = Id::generate();
            sqlx::query(
                "INSERT INTO channels (id, kind, name, position, created_by, created_at)
                 VALUES (?, ?, ?, 0, ?, ?)",
            )
            .bind(id)
            .bind(ChannelKind::GroupDm.as_i64())
            .bind(name)
            .bind(created_by)
            .bind(now)
            .execute(&mut *tx)
            .await
            .unwrap();
            let mut query = sqlx::query(sqlx::AssertSqlSafe(insert_members.clone()));
            for member in members {
                query = query.bind(id).bind(*member);
            }
            query.execute(&mut *tx).await.unwrap();
            ids.push(id);
        }
        tx.commit().await.unwrap();
        ids
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

#[tokio::test]
async fn a_member_in_thousands_of_group_dms_still_logs_in() {
    let mut world = World::new().await;
    let (ada, token) = world.account("ada").await;
    let mut crowd = vec![ada];
    for n in 0..9 {
        crowd.push(world.bare(&format!("crowd{n}")).await);
    }
    // About 300 bytes each on the wire, so the old Welcome passed 1 MiB.
    let name = "x".repeat(64);
    let groups = world.seed_groups(4000, crowd[1], &crowd, &name).await;
    // The oldest conversation is the most recently active one.
    let busiest = groups[0];
    {
        let mut conn = world.pool.write().await;
        messages::insert(
            &mut conn,
            NewMessage {
                id: Id::generate(),
                channel_id: busiest,
                author_id: Some(crowd[1]),
                webhook_id: None,
                webhook_name: None,
                content: "still here".into(),
                reply_to: None,
                mentions: Vec::new(),
                mentions_everyone: false,
            },
        )
        .await
        .unwrap();
    }

    let (_conn, mut stream) = world.connect().await;
    let welcome = token_hello(&mut stream, &token).await;
    let snapshot = welcome.snapshot.unwrap();
    assert!(snapshot.direct_messages.len() <= limits::WELCOME_DMS);
    assert_eq!(snapshot.direct_messages[0].id, id_to_bytes(busiest));
    let mut cursor = snapshot
        .direct_messages_next
        .clone()
        .expect("truncated DM list names a cursor");

    let mut seen: BTreeSet<Bytes> = snapshot
        .direct_messages
        .iter()
        .map(|channel| channel.id.clone())
        .collect();
    let mut request = 2;
    loop {
        let page = dm_page(
            &command(
                &mut stream,
                request,
                Msg::ListDms(v1::ListDms {
                    cursor: Some(cursor.clone()),
                    limit: 0,
                }),
            )
            .await,
        );
        request += 1;
        assert!(page.channels.len() <= limits::DM_PAGE);
        for channel in &page.channels {
            assert!(seen.insert(channel.id.clone()), "a page repeats a DM");
            assert_eq!(channel.member_ids.len(), crowd.len());
        }
        match page.next {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(seen.len(), groups.len());
}

#[tokio::test]
async fn client_core_loads_the_conversations_a_welcome_left_out() {
    use ss_client_core::{Client, Credentials, ServerAddr};
    let world = World::new().await;
    let client = Client::connect(
        ServerAddr::new("127.0.0.1", world.port).unwrap(),
        Credentials::Register(v1::Register {
            invite_code: world.code.clone(),
            username: "ada".into(),
            password: PASSWORD.into(),
        }),
        world.pins.clone(),
    )
    .await
    .unwrap();
    let ada = id_from_bytes(&client.state().read().own_user.clone().unwrap().id).unwrap();
    let bea = world.bare("bea").await;
    let cyd = world.bare("cyd").await;
    let total = limits::WELCOME_DMS + 5;
    world.seed_groups(total, bea, &[ada, bea, cyd], "").await;
    let client = Client::connect(
        ServerAddr::new("127.0.0.1", world.port).unwrap(),
        Credentials::Login(v1::Login {
            username: "ada".into(),
            password: PASSWORD.into(),
        }),
        world.pins.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        client.state().read().direct_messages.len(),
        limits::WELCOME_DMS
    );
    assert!(client.state().read().direct_messages_next.is_some());
    assert!(!client.list_dms().await.unwrap(), "one page holds the rest");
    assert_eq!(client.state().read().direct_messages.len(), total);
    assert!(client.state().read().direct_messages_next.is_none());
    assert!(!client.list_dms().await.unwrap());
}

#[tokio::test]
async fn group_dms_are_capped_per_creator_and_per_member() {
    let mut world = World::new().await;
    let (ada, ada_token) = world.account("ada").await;
    let (bea, bea_token) = world.account("bea").await;
    let cyd = world.bare("cyd").await;
    let dee = world.bare("dee").await;
    let eve = world.bare("eve").await;
    let fay = world.bare("fay").await;

    // Ada is already in as many group conversations as anyone may be.
    world
        .seed_groups(limits::GROUP_DMS_PER_MEMBER, eve, &[eve, ada, fay], "")
        .await;
    let (_bea_conn, mut bea_stream) = world.connect().await;
    token_hello(&mut bea_stream, &bea_token).await;
    let full = command(&mut bea_stream, 2, create_dm(&[ada, cyd])).await;
    assert_eq!(
        error_of(&full),
        (forbidden(), "user is in too many group conversations")
    );
    let group = dm_channel(&command(&mut bea_stream, 3, create_dm(&[cyd, dee])).await);
    let added = command(
        &mut bea_stream,
        4,
        Msg::AddDmMembers(v1::AddDmMembers {
            channel_id: group.id.clone(),
            user_ids: vec![id_to_bytes(ada)],
        }),
    )
    .await;
    assert_eq!(
        error_of(&added),
        (forbidden(), "user is in too many group conversations")
    );
    // A 1:1 conversation is unique per pair and is not capped.
    let pair = dm_channel(&command(&mut bea_stream, 5, create_dm(&[ada])).await);
    assert_eq!(pair.kind, i32::from(v1::ChannelKind::Dm));
    // At the cap, Ada still logs in.
    let (_ada_conn, mut ada_stream) = world.connect().await;
    token_hello(&mut ada_stream, &ada_token).await;

    // Bea has started one group; fill her up to one below the creator cap.
    world
        .seed_groups(limits::GROUP_DMS_PER_CREATOR - 2, bea, &[bea, cyd, dee], "")
        .await;
    let last = command(&mut bea_stream, 6, create_dm(&[cyd, dee])).await;
    assert!(matches!(last.msg, Some(Event::Ack(_))), "{last:?}");
    let over = command(&mut bea_stream, 7, create_dm(&[cyd, dee])).await;
    assert_eq!(
        error_of(&over),
        (forbidden(), "you have started too many group conversations")
    );
}

#[tokio::test]
async fn opening_new_conversations_has_its_own_quota() {
    let mut world = World::new().await;
    let (_ada, token) = world.account("ada").await;
    let mut people = Vec::new();
    for n in 0..=limits::DM_CREATE_BURST {
        people.push(world.bare(&format!("friend{n}")).await);
    }
    let (_conn, mut stream) = world.connect().await;
    token_hello(&mut stream, &token).await;
    let mut request = 2;
    for person in people.iter().take(people.len() - 1) {
        let reply = command(&mut stream, request, create_dm(&[*person])).await;
        assert!(matches!(reply.msg, Some(Event::Ack(_))), "{reply:?}");
        request += 1;
    }
    let limited = command(&mut stream, request, create_dm(&[people[people.len() - 1]])).await;
    match &limited.msg {
        Some(Event::Error(error)) => {
            assert_eq!(error.code, i32::from(v1::ErrorCode::RateLimited));
            assert!(error.retry_after_ms.is_some());
        }
        other => panic!("expected RATE_LIMITED, got {other:?}"),
    }
    // Reopening a conversation that exists is free.
    let again = command(&mut stream, request + 1, create_dm(&[people[0]])).await;
    assert!(matches!(again.msg, Some(Event::Ack(_))), "{again:?}");
}

#[tokio::test]
async fn an_older_conversation_is_announced_before_its_new_message() {
    let mut world = World::new().await;
    let (ada, ada_token) = world.account("ada").await;
    let (bea, bea_token) = world.account("bea").await;
    let cyd = world.bare("cyd").await;
    let groups = world
        .seed_groups(limits::WELCOME_DMS + 5, bea, &[ada, bea, cyd], "")
        .await;
    // Seeded in the same millisecond, so the smallest id is the least active.
    let old = id_to_bytes(*groups.iter().min().unwrap());

    let (_ada_conn, mut ada_stream) = world.connect().await;
    let welcome = token_hello(&mut ada_stream, &ada_token).await;
    let snapshot = welcome.snapshot.unwrap();
    assert!(snapshot.direct_messages_next.is_some());
    assert!(snapshot.direct_messages.iter().all(|dm| dm.id != old));

    let (_bea_conn, mut bea_stream) = world.connect().await;
    token_hello(&mut bea_stream, &bea_token).await;
    let sent = command(
        &mut bea_stream,
        2,
        Msg::SendMessage(v1::SendMessage {
            channel_id: old.clone(),
            content: "remember this one?".into(),
            ..Default::default()
        }),
    )
    .await;
    assert!(matches!(sent.msg, Some(Event::Ack(_))), "{sent:?}");

    let mut announced = false;
    loop {
        let frame = recv(&mut ada_stream).await;
        match frame.msg {
            Some(Event::ChannelCreated(created)) => {
                let channel = created.channel.unwrap();
                if channel.id == old {
                    assert_eq!(channel.member_ids.len(), 3);
                    announced = true;
                }
            }
            Some(Event::MessageCreated(created)) => {
                assert_eq!(created.message.unwrap().channel_id, old);
                assert!(announced, "message arrived for an unknown conversation");
                break;
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_dm_rename_does_not_recompute_other_sessions_channel_views() {
    let mut world = World::new().await;
    let (_ada, ada_token) = world.account("ada").await;
    let (bea, bea_token) = world.account("bea").await;
    let (_cyd, cyd_token) = world.account("cyd").await;
    let eve = world.bare("eve").await;
    let (_a, mut ada) = world.connect().await;
    let welcome = token_hello(&mut ada, &ada_token).await;
    let general = welcome
        .snapshot
        .unwrap()
        .channels
        .into_iter()
        .find(|channel| channel.kind == i32::from(v1::ChannelKind::Text))
        .unwrap()
        .id;
    let (_b, mut bea_stream) = world.connect().await;
    token_hello(&mut bea_stream, &bea_token).await;
    let (_c, mut outsider) = world.connect().await;
    token_hello(&mut outsider, &cyd_token).await;

    let group = dm_channel(&command(&mut ada, 2, create_dm(&[bea, eve])).await);
    let created = next_channel(&mut bea_stream).await;
    assert_eq!(created.id, group.id);
    settle(&mut ada, &mut bea_stream, &mut outsider, &general, 3).await;

    let before = world.server.channel_view_count();
    let renamed = command(
        &mut ada,
        4,
        Msg::UpdateChannel(v1::UpdateChannel {
            channel_id: group.id.clone(),
            name: Some("weekend".into()),
            ..Default::default()
        }),
    )
    .await;
    assert!(matches!(renamed.msg, Some(Event::Ack(_))), "{renamed:?}");
    let updated = next_channel(&mut bea_stream).await;
    assert_eq!(updated.name, "weekend");
    let outsider_saw = settle(&mut ada, &mut bea_stream, &mut outsider, &general, 5).await;
    assert!(
        outsider_saw.iter().all(|msg| !matches!(
            msg,
            Event::ChannelCreated(_) | Event::ChannelUpdated(_) | Event::ChannelDeleted(_)
        )),
        "{outsider_saw:?}"
    );
    assert_eq!(world.server.channel_view_count(), before);
}

/// Posts in `general` and waits until both other sessions saw it, so every
/// event published before it has been handled. Returns what the outsider got.
async fn settle(
    ada: &mut ss_net::control::ClientStream,
    bea: &mut ss_net::control::ClientStream,
    outsider: &mut ss_net::control::ClientStream,
    general: &Bytes,
    request: u32,
) -> Vec<Event> {
    let sent = command(
        ada,
        request,
        Msg::SendMessage(v1::SendMessage {
            channel_id: general.clone(),
            content: format!("marker {request}"),
            ..Default::default()
        }),
    )
    .await;
    assert!(matches!(sent.msg, Some(Event::Ack(_))), "{sent:?}");
    until_message(bea).await;
    until_message(outsider).await
}

async fn until_message(stream: &mut ss_net::control::ClientStream) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        match recv(stream).await.msg {
            Some(Event::MessageCreated(_)) => return seen,
            Some(other) => seen.push(other),
            None => {}
        }
    }
}

#[tokio::test]
async fn deleting_a_large_category_keeps_observers_connected() {
    let mut world = World::new().await;
    let (ada, ada_token) = world.account("ada").await;
    let (_bea, bea_token) = world.account("bea").await;
    let category = Id::generate();
    // More than the event bus holds, so only one trigger may be published.
    let children: usize = 1100;
    {
        let mut guard = world.pool.write().await;
        let mut tx = guard.begin().await.unwrap();
        let now = Timestamp::now().as_millis();
        let insert =
            "INSERT INTO channels (id, kind, name, parent_id, position, created_by, created_at)
                      VALUES (?, ?, ?, ?, ?, ?, ?)";
        sqlx::query(insert)
            .bind(category)
            .bind(ChannelKind::Category.as_i64())
            .bind("archive")
            .bind(None::<Id>)
            .bind(5_i64)
            .bind(ada)
            .bind(now)
            .execute(&mut *tx)
            .await
            .unwrap();
        for position in 0..i64::try_from(children).unwrap() {
            sqlx::query(insert)
                .bind(Id::generate())
                .bind(ChannelKind::Text.as_i64())
                .bind(format!("old-{position}"))
                .bind(category)
                .bind(position)
                .bind(ada)
                .bind(now)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
    }
    let (_a, mut admin) = world.connect().await;
    token_hello(&mut admin, &ada_token).await;
    let (observer_conn, mut observer) = world.connect().await;
    let welcome = token_hello(&mut observer, &bea_token).await;
    assert!(welcome.snapshot.unwrap().channels.len() > children);

    let deleted = command(
        &mut admin,
        2,
        Msg::DeleteChannel(v1::DeleteChannel {
            channel_id: id_to_bytes(category),
        }),
    )
    .await;
    assert!(matches!(deleted.msg, Some(Event::Ack(_))), "{deleted:?}");
    let mut gone = 0;
    while gone <= children {
        match recv(&mut observer).await.msg {
            Some(Event::ChannelDeleted(_)) => gone += 1,
            Some(Event::ServerNotice(notice)) => panic!("observer kicked: {}", notice.text),
            _ => {}
        }
    }
    let pong = command(&mut observer, 2, Msg::Ping(v1::Ping { sent_at: 7 })).await;
    assert!(matches!(pong.msg, Some(Event::Pong(_))), "{pong:?}");
    assert!(observer_conn.quic().close_reason().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_full_outbound_queue_closes_the_connection_instead_of_dropping_a_frame() {
    let mut world = World::new().await;
    let (ada, token) = world.account("ada").await;
    let (slow_conn, mut slow) = world.connect().await;
    token_hello(&mut slow, &token).await;
    let conn = world.server.registry().connection_ids(ada)[0];
    // Long enough that only the full queue, not the stall timer, can close it.
    world
        .server
        .registry()
        .set_slow_after(Duration::from_secs(60));
    // The writer shares this current-thread runtime and is not polled here.
    for n in 0..=registry::OUTBOUND_CAPACITY {
        world
            .server
            .registry()
            .enqueue(conn, notice(&format!("{n}")));
    }
    let error = timeout(Duration::from_secs(3), slow_conn.quic().closed())
        .await
        .expect("a full queue must close the connection, not drop the frame");
    match error {
        quinn::ConnectionError::ApplicationClosed(close) => assert_eq!(
            close.error_code,
            quinn::VarInt::from_u32(registry::CLOSE_SLOW)
        ),
        other => panic!("unexpected close: {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_client_whose_queue_overflowed_is_complete_after_it_reconnects() {
    use ss_client_core::{Client, ConnectionState, Credentials, ServerAddr};
    let world = World::new().await;
    let address = ServerAddr::new("127.0.0.1", world.port).unwrap();
    let client = Client::connect(
        address,
        Credentials::Register(v1::Register {
            invite_code: world.code.clone(),
            username: "ada".into(),
            password: PASSWORD.into(),
        }),
        world.pins.clone(),
    )
    .await
    .unwrap();
    let mut events = client.subscribe();
    let bea = world.bare("bea").await;
    let own = id_from_bytes(&client.state().read().own_user.clone().unwrap().id).unwrap();
    let conn = world.server.registry().connection_ids(own)[0];
    world
        .server
        .registry()
        .set_slow_after(Duration::from_secs(60));

    // Bea's new name reaches only the database and the frame that would be
    // dropped. After the overflow the client must learn it from Welcome.
    let renamed = {
        let mut conn = world.pool.write().await;
        users::set_profile(
            &mut conn,
            bea,
            UserProfile {
                display_name: "Beatrice".into(),
                avatar_attachment_id: None,
            },
        )
        .await
        .unwrap()
    };
    for n in 0..registry::OUTBOUND_CAPACITY {
        world
            .server
            .registry()
            .enqueue(conn, notice(&format!("{n}")));
    }
    world.server.registry().enqueue(
        conn,
        v1::ServerEnvelope {
            request_id: 0,
            msg: Some(Event::MemberUpdated(v1::MemberUpdated {
                member: Some(v1::Member {
                    user: Some(v1::User {
                        id: id_to_bytes(bea),
                        username: renamed.username.clone(),
                        display_name: renamed.display_name.clone(),
                        avatar_attachment_id: None,
                        kind: i32::from(v1::UserKind::User),
                    }),
                    roles: Vec::new(),
                    presence: None,
                }),
            })),
        },
    );
    let named = |state: &ss_client_core::State| {
        state.connection == ConnectionState::Connected
            && state.members.get(&bea).is_some_and(|member| {
                member
                    .user
                    .as_ref()
                    .is_some_and(|user| user.display_name == "Beatrice")
            })
    };
    timeout(Duration::from_secs(15), async {
        while !named(&client.state().read()) {
            let _ = events.recv().await;
        }
    })
    .await
    .expect("client state after the overflow");
    assert_ne!(
        world.server.registry().connection_ids(own),
        vec![conn],
        "the overflowing connection was replaced"
    );
}

#[tokio::test]
async fn a_slow_command_does_not_cost_the_session_its_events() {
    let mut world = World::new().await;
    let (_ada, ada_token) = world.account("ada").await;
    let mut spammers = Vec::new();
    for n in 0..3 {
        spammers.push(world.account(&format!("spam{n}")).await.1);
    }
    let (actor_conn, mut actor) = world.connect().await;
    let welcome = token_hello(&mut actor, &ada_token).await;
    let general = welcome
        .snapshot
        .unwrap()
        .channels
        .into_iter()
        .find(|channel| channel.kind == i32::from(v1::ChannelKind::Text))
        .unwrap()
        .id;
    let mut streams = Vec::new();
    for token in &spammers {
        let (conn, mut stream) = world.connect().await;
        token_hello(&mut stream, token).await;
        streams.push((conn, stream));
    }

    // The actor's message waits on the database writer.
    let writer = world.pool.write().await;
    actor
        .send(&v1::ClientEnvelope {
            request_id: 2,
            msg: Some(Msg::SendMessage(v1::SendMessage {
                channel_id: general,
                content: "slow".into(),
                ..Default::default()
            })),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // More events than the bus holds arrive meanwhile.
    let per_spammer = 95;
    for (_, stream) in &mut streams {
        for n in 0..per_spammer {
            let reply = command(
                stream,
                n + 2,
                Msg::SetPresence(v1::SetPresence {
                    status: i32::from(v1::PresenceStatus::Online),
                    activity: format!("busy {n}"),
                }),
            )
            .await;
            assert!(matches!(reply.msg, Some(Event::Ack(_))), "{reply:?}");
        }
    }
    drop(writer);

    let expected = usize::try_from(per_spammer).unwrap() * streams.len();
    let mut presence = 0;
    let mut acked = false;
    while presence < expected || !acked {
        let frame = recv(&mut actor).await;
        match frame.msg {
            Some(Event::PresenceUpdated(_)) => presence += 1,
            Some(Event::Ack(_)) if frame.request_id == 2 => acked = true,
            Some(Event::ServerNotice(notice)) => panic!("actor kicked: {}", notice.text),
            _ => {}
        }
    }
    assert!(actor_conn.quic().close_reason().is_none());
}

#[tokio::test]
async fn a_token_hello_does_not_wait_for_the_writer() {
    let mut world = World::new().await;
    let (_ada, token) = world.account("ada").await;
    let (_first, mut first) = world.connect().await;
    token_hello(&mut first, &token).await;
    let writer = world.pool.write().await;
    let (_second, mut second) = world.connect().await;
    let welcome = timeout(Duration::from_secs(3), token_hello(&mut second, &token))
        .await
        .expect("token Hello queued behind the writer");
    assert!(welcome.user.is_some());
    drop(writer);
}

fn notice(text: &str) -> v1::ServerEnvelope {
    v1::ServerEnvelope {
        request_id: 0,
        msg: Some(Event::ServerNotice(v1::ServerNotice { text: text.into() })),
    }
}

async fn token_hello(stream: &mut ss_net::control::ClientStream, token: &str) -> v1::Welcome {
    let reply = command(
        stream,
        1,
        Msg::Hello(v1::Hello {
            protocol_version: 1,
            client_name: "test".into(),
            auth: Some(v1::hello::Auth::SessionToken(token.into())),
            ..Default::default()
        }),
    )
    .await;
    match reply.msg {
        Some(Event::Welcome(welcome)) => welcome,
        other => panic!("expected Welcome, got {other:?}"),
    }
}

async fn recv(stream: &mut ss_net::control::ClientStream) -> v1::ServerEnvelope {
    timeout(DEADLINE, stream.recv()).await.unwrap().unwrap()
}

async fn command(
    stream: &mut ss_net::control::ClientStream,
    request_id: u32,
    msg: Msg,
) -> v1::ServerEnvelope {
    stream
        .send(&v1::ClientEnvelope {
            request_id,
            msg: Some(msg),
        })
        .await
        .unwrap();
    loop {
        let frame = recv(stream).await;
        if frame.request_id == request_id {
            return frame;
        }
    }
}

async fn next_channel(stream: &mut ss_net::control::ClientStream) -> v1::Channel {
    loop {
        match recv(stream).await.msg {
            Some(Event::ChannelCreated(event)) => return event.channel.unwrap(),
            Some(Event::ChannelUpdated(event)) => return event.channel.unwrap(),
            _ => {}
        }
    }
}

fn create_dm(users: &[Id]) -> Msg {
    Msg::CreateDm(v1::CreateDm {
        user_ids: users.iter().copied().map(id_to_bytes).collect(),
    })
}

fn dm_channel(envelope: &v1::ServerEnvelope) -> v1::Channel {
    match &envelope.msg {
        Some(Event::Ack(v1::Ack {
            result: Some(v1::ack::Result::DmChannel(channel)),
            ..
        })) => channel.clone(),
        other => panic!("expected a DM channel, got {other:?}"),
    }
}

fn dm_page(envelope: &v1::ServerEnvelope) -> v1::DmPage {
    match &envelope.msg {
        Some(Event::Ack(v1::Ack {
            result: Some(v1::ack::Result::DmPage(page)),
            ..
        })) => page.clone(),
        other => panic!("expected a DM page, got {other:?}"),
    }
}

fn error_of(envelope: &v1::ServerEnvelope) -> (i32, &str) {
    match &envelope.msg {
        Some(Event::Error(error)) => (error.code, error.message.as_str()),
        other => panic!("expected Error, got {other:?}"),
    }
}

fn forbidden() -> i32 {
    i32::from(v1::ErrorCode::Forbidden)
}
