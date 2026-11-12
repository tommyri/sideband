//! Bot accounts (T-080): tokens, scopes, both transports, the event stream,
//! limits, and the two example bots.

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use futures::StreamExt;
use serde_json::{Value, json};
use ss_client_core::{Client, ClientEvent, Credentials, Error, ServerAddr, v1};
use ss_core::{Id, Permissions};
use ss_net::{
    pins::{MemoryPinStore, PinStore},
    server::Endpoint,
    tls,
};
use ss_proto::validate::{id_from_bytes, id_to_bytes as wire};
use ss_server::{auth, connection::Server, db};
use tokio::{task::JoinHandle, time::timeout};
use tower::ServiceExt;

const WAIT: Duration = Duration::from_secs(10);
const PASSWORD: &str = "test-password";

struct World {
    dir: tempfile::TempDir,
    pool: Arc<db::Pool>,
    endpoint: Endpoint,
    server: Arc<Server>,
    worker: JoinHandle<()>,
    address: ServerAddr,
    pins: Arc<MemoryPinStore>,
    fingerprint: String,
    invite: String,
}

impl World {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(db::Pool::open(dir.path().join("test.db")).await.unwrap());
        let invite = auth::ensure_owner_invite(&pool).await.unwrap().unwrap();
        let identity = tls::generate_self_signed(dir.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let address = ServerAddr::new("127.0.0.1", endpoint.local_addr().unwrap().port()).unwrap();
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(&address.id().unwrap(), fingerprint).unwrap();
        let server = Arc::new(Server::open(Arc::clone(&pool)).await.unwrap());
        let worker = tokio::spawn(server.clone().run(endpoint.clone()));
        Self {
            dir,
            pool,
            endpoint,
            server,
            worker,
            address,
            pins,
            fingerprint: fingerprint.to_string(),
            invite,
        }
    }

    async fn owner(&self) -> Client {
        self.connect(Credentials::Register(v1::Register {
            invite_code: self.invite.clone(),
            username: "owner".into(),
            password: PASSWORD.into(),
        }))
        .await
        .unwrap()
    }

    async fn member(&self, owner: &Client, username: &str) -> Client {
        let invite = owner
            .create_invite(v1::CreateInvite {
                max_uses: Some(1),
                expires_at: None,
            })
            .await
            .unwrap();
        self.connect(Credentials::Register(v1::Register {
            invite_code: invite.code,
            username: username.into(),
            password: PASSWORD.into(),
        }))
        .await
        .unwrap()
    }

    async fn connect(&self, credentials: Credentials) -> Result<Client, Error> {
        Client::connect(self.address.clone(), credentials, self.pins.clone()).await
    }

    /// One raw `Hello` and the server's answer.
    async fn hello(&self, auth: v1::hello::Auth) -> v1::ServerEnvelope {
        let mut connection =
            ss_net::client::connect(&self.address.host, self.address.port, self.pins.clone())
                .await
                .unwrap();
        let mut stream = connection.open_control().await.unwrap();
        stream
            .send(&v1::ClientEnvelope {
                request_id: 1,
                msg: Some(v1::client_envelope::Msg::Hello(v1::Hello {
                    protocol_version: 1,
                    client_name: "test".into(),
                    auth: Some(auth),
                    ..Default::default()
                })),
            })
            .await
            .unwrap();
        timeout(WAIT, stream.recv()).await.unwrap().unwrap()
    }

    /// One HTTPS request through the public router.
    async fn rest(
        &self,
        method: &str,
        path: &str,
        authorization: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, authorization);
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
        let response = self
            .server
            .public_routes()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn events(&self, token: &str, query: &str) -> Events {
        let response = self
            .server
            .public_routes()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/events{query}"))
                    .header(header::AUTHORIZATION, format!("Bot {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "event stream refused");
        Events {
            body: Box::pin(response.into_body().into_data_stream()),
            buffer: String::new(),
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test done");
        self.worker.abort();
    }
}

type DataStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Send>>;

/// A server-sent event stream, read one `data:` line at a time.
struct Events {
    body: DataStream,
    buffer: String,
}

impl Events {
    /// The next event, or `None` when the stream ended.
    async fn next(&mut self) -> Option<Value> {
        timeout(WAIT, async {
            loop {
                while let Some(end) = self.buffer.find('\n') {
                    let line: String = self.buffer.drain(..=end).collect();
                    if let Some(data) = line.trim_end().strip_prefix("data:") {
                        return Some(serde_json::from_str(data.trim()).unwrap());
                    }
                }
                let chunk = self.body.next().await?.ok()?;
                self.buffer.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        })
        .await
        .expect("no event in time")
    }

    /// Skips to the next event of `kind` (its JSON key).
    async fn next_of(&mut self, kind: &str) -> Value {
        loop {
            let event = self.next().await.expect("stream ended");
            if let Some(value) = event.get(kind) {
                return value.clone();
            }
        }
    }
}

fn general(client: &Client) -> Id {
    named(client, "general", v1::ChannelKind::Text)
}

fn named(client: &Client, name: &str, kind: v1::ChannelKind) -> Id {
    *client
        .state()
        .read()
        .channels
        .iter()
        .find(|(_, channel)| channel.name == name && channel.kind == i32::from(kind))
        .unwrap()
        .0
}

async fn create_bot(owner: &Client, username: &str, scopes: Permissions) -> (Id, String) {
    let bot = owner
        .create_bot(v1::CreateBot {
            username: username.into(),
            display_name: String::new(),
            scopes: scopes.bits(),
        })
        .await
        .unwrap();
    let user = bot.user.unwrap();
    assert_eq!(user.kind, i32::from(v1::UserKind::Bot));
    (id_from_bytes(&user.id).unwrap(), bot.token.unwrap())
}

const CHAT: Permissions = Permissions::VIEW_CHANNEL.union(Permissions::SEND_MESSAGES);

fn post(content: &str) -> Value {
    json!({ "content": content })
}

fn code(error: &Error) -> i32 {
    match error {
        Error::Server { code, .. } => *code,
        other => panic!("expected a server error, got {other}"),
    }
}

fn hello_error(envelope: &v1::ServerEnvelope) -> v1::ErrorCode {
    match &envelope.msg {
        Some(v1::server_envelope::Msg::Error(error)) => {
            v1::ErrorCode::try_from(error.code).unwrap()
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

async fn message_from(client: &Client, author: Id, channel: Id) -> v1::Message {
    let mut events = client.subscribe();
    timeout(WAIT, async {
        loop {
            {
                let state = client.state();
                let state = state.read();
                if let Some(found) = state.messages.get(&channel).and_then(|cache| {
                    cache
                        .ordered()
                        .into_iter()
                        .rev()
                        .find(|m| m.author.as_ref().is_some_and(|a| a.id == wire(author)))
                        .cloned()
                }) {
                    return found;
                }
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("no message from the bot")
}

#[tokio::test]
async fn a_bot_token_and_a_session_token_never_stand_in_for_each_other() {
    let world = World::new().await;
    let owner = world.owner().await;
    let (bot, token) = create_bot(&owner, "dice", CHAT).await;
    let session = owner_session(&world).await;

    // QUIC: the bot token is not a session token, nor the reverse.
    for auth in [
        v1::hello::Auth::SessionToken(token.clone()),
        v1::hello::Auth::ResumeToken(token.clone()),
        v1::hello::Auth::BotToken(session.clone()),
    ] {
        assert_eq!(
            hello_error(&world.hello(auth).await),
            v1::ErrorCode::Unauthenticated
        );
    }
    // A bot has no password: its name logs in like an unknown one.
    assert_eq!(
        hello_error(
            &world
                .hello(v1::hello::Auth::Login(v1::Login {
                    username: "dice".into(),
                    password: "!bot".into(),
                }))
                .await
        ),
        v1::ErrorCode::Unauthenticated
    );
    let Some(v1::server_envelope::Msg::Welcome(welcome)) = world
        .hello(v1::hello::Auth::BotToken(token.clone()))
        .await
        .msg
    else {
        panic!("a bot token did not sign the bot in");
    };
    let user = welcome.user.unwrap();
    assert_eq!(user.id, wire(bot));
    assert_eq!(user.kind, i32::from(v1::UserKind::Bot));

    // HTTPS: user routes refuse the bot token, bot routes refuse sessions.
    let history = format!("/api/v1/channels/{}/messages", general(&owner));
    let (status, _) = world
        .rest("GET", &history, &format!("Bearer {token}"), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for authorization in [format!("Bot {session}"), format!("Bearer {token}")] {
        let (status, _) = world
            .rest("POST", &history, &authorization, Some(post("hi")))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{authorization}");
    }
    let (status, _) = world
        .rest("POST", &history, &format!("Bot {token}"), Some(post("hi")))
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// The owner's session token, from a fresh password login.
async fn owner_session(world: &World) -> String {
    let Some(v1::server_envelope::Msg::Welcome(welcome)) = world
        .hello(v1::hello::Auth::Login(v1::Login {
            username: "owner".into(),
            password: PASSWORD.into(),
        }))
        .await
        .msg
    else {
        panic!("owner login failed");
    };
    welcome.resume_token
}

#[tokio::test]
async fn a_revoked_token_is_refused_at_once_on_every_transport() {
    let world = World::new().await;
    let owner = world.owner().await;
    let (bot, token) = create_bot(&owner, "dice", CHAT).await;
    let native = world
        .connect(Credentials::Bot(token.clone()))
        .await
        .unwrap();
    let mut native_events = native.subscribe();
    let mut stream = world.events(&token, "").await;
    assert!(stream.next().await.unwrap().get("welcome").is_some());

    owner
        .revoke_bot_token(v1::RevokeBotToken { bot_id: wire(bot) })
        .await
        .unwrap();

    // The open stream says so and ends.
    stream.next_of("sessionRevoked").await;
    assert!(stream.next().await.is_none(), "the stream stayed open");
    // The QUIC connection is closed as signed out and does not reconnect.
    timeout(WAIT, async {
        loop {
            if let ClientEvent::Removed { banned: false, .. } = native_events.recv().await.unwrap()
            {
                break;
            }
        }
    })
    .await
    .expect("the bot's QUIC connection stayed open");
    // New requests are refused.
    let path = format!("/api/v1/channels/{}/messages", general(&owner));
    let (status, _) = world
        .rest("POST", &path, &format!("Bot {token}"), Some(post("hi")))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        hello_error(&world.hello(v1::hello::Auth::BotToken(token)).await),
        v1::ErrorCode::Unauthenticated
    );

    // A reset issues a working token; the one before it stops at once.
    let first = owner
        .reset_bot_token(v1::ResetBotToken {
            bot_id: wire(bot),
            scopes: CHAT.bits(),
        })
        .await
        .unwrap()
        .token
        .unwrap();
    let mut stream = world.events(&first, "").await;
    stream.next_of("welcome").await;
    let second = owner
        .reset_bot_token(v1::ResetBotToken {
            bot_id: wire(bot),
            scopes: CHAT.bits(),
        })
        .await
        .unwrap()
        .token
        .unwrap();
    stream.next_of("sessionRevoked").await;
    let (status, _) = world
        .rest("POST", &path, &format!("Bot {first}"), Some(post("hi")))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = world
        .rest("POST", &path, &format!("Bot {second}"), Some(post("hi")))
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn scopes_and_overrides_limit_a_bot_on_both_transports() {
    let world = World::new().await;
    let owner = world.owner().await;
    let channel = general(&owner);
    let path = format!("/api/v1/channels/{channel}/messages");

    // @everyone may send, but the token's scopes do not include it.
    let (bot, token) = create_bot(&owner, "reader", Permissions::VIEW_CHANNEL).await;
    let (status, body) = world
        .rest("POST", &path, &format!("Bot {token}"), Some(post("hi")))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "ERROR_CODE_FORBIDDEN");
    let native = world
        .connect(Credentials::Bot(token.clone()))
        .await
        .unwrap();
    let refused = native
        .send_message(v1::SendMessage {
            channel_id: wire(channel),
            content: "hi".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(&refused), i32::from(v1::ErrorCode::Forbidden));
    assert_eq!(
        native.state().read().permissions(channel),
        Some(Permissions::VIEW_CHANNEL)
    );
    drop(native);

    // With the scope, a channel override still denies it.
    let (_, writer) = create_bot(&owner, "writer", CHAT).await;
    let (status, sent) = world
        .rest("POST", &path, &format!("Bot {writer}"), Some(post("hello")))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(sent["messageId"].is_string(), "{sent}");
    owner
        .set_channel_override(v1::SetChannelOverride {
            channel_id: wire(channel),
            role_id: wire(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: Permissions::SEND_MESSAGES.bits(),
        })
        .await
        .unwrap();
    let (status, _) = world
        .rest("POST", &path, &format!("Bot {writer}"), Some(post("again")))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let native = world.connect(Credentials::Bot(writer)).await.unwrap();
    let refused = native
        .send_message(v1::SendMessage {
            channel_id: wire(channel),
            content: "again".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(&refused), i32::from(v1::ErrorCode::Forbidden));

    // Scopes cap roles too: the Admin role without the scope is not admin.
    owner
        .set_user_roles(v1::SetUserRoles {
            user_id: wire(bot),
            role_ids: vec![wire(db::ADMIN_ROLE_ID)],
        })
        .await
        .unwrap();
    let (status, _) = world
        .rest("POST", &path, &format!("Bot {token}"), Some(post("x")))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_bot_the_operator_cli_creates_is_capped_and_revoked_like_any_other() {
    use ss_server::handlers::bots;
    let world = World::new().await;
    let owner = world.owner().await;
    let path = format!("/api/v1/channels/{}/messages", general(&owner));
    // Another process wrote these rows; this server's snapshot has not seen them.
    let (user, _, token) =
        bots::create_account(&world.pool, None, "cli.bot", "", Permissions::VIEW_CHANNEL)
            .await
            .unwrap();
    let (status, _) = world
        .rest("POST", &path, &format!("Bot {}", *token), Some(post("hi")))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "an unseen bot ran uncapped");

    let native = world
        .connect(Credentials::Bot(token.to_string()))
        .await
        .unwrap();
    let mut events = native.subscribe();
    let (old, _, fresh) = bots::replace_token(&world.pool, None, user.id, CHAT)
        .await
        .unwrap();
    assert!(old.is_some());
    let (status, _) = world
        .rest("POST", &path, &format!("Bot {}", *token), Some(post("hi")))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = world
        .rest("POST", &path, &format!("Bot {}", *fresh), Some(post("hi")))
        .await;
    assert_eq!(status, StatusCode::OK);
    // The running server closes the old token's connection on its sweep.
    assert_eq!(world.server.sessions().sweep().await.unwrap(), 1);
    timeout(WAIT, async {
        while !matches!(events.recv().await, Ok(ClientEvent::Removed { .. })) {}
    })
    .await
    .expect("the sweep left the revoked bot connected");
}

#[tokio::test]
async fn only_managers_create_bots_and_only_with_bits_they_hold() {
    let world = World::new().await;
    let owner = world.owner().await;
    let ada = world.member(&owner, "ada").await;
    let refused = ada
        .create_bot(v1::CreateBot {
            username: "helper".into(),
            display_name: String::new(),
            scopes: CHAT.bits(),
        })
        .await
        .unwrap_err();
    assert_eq!(code(&refused), i32::from(v1::ErrorCode::Forbidden));

    let managers = owner
        .create_role(v1::CreateRole {
            name: "Managers".into(),
            color: None,
            permissions: Permissions::MANAGE_SERVER.union(CHAT).bits(),
            position: 2,
        })
        .await
        .unwrap();
    let ada_id = id_from_bytes(&ada.state().read().own_user.as_ref().unwrap().id).unwrap();
    owner
        .set_user_roles(v1::SetUserRoles {
            user_id: wire(ada_id),
            role_ids: vec![managers.id.clone()],
        })
        .await
        .unwrap();
    let mut events = ada.subscribe();
    timeout(WAIT, async {
        while ada
            .create_bot(v1::CreateBot {
                username: "probe".into(),
                display_name: String::new(),
                scopes: 0,
            })
            .await
            .is_err()
        {
            let _ = events.recv().await;
        }
    })
    .await
    .unwrap();
    let escalation = ada
        .create_bot(v1::CreateBot {
            username: "banhammer".into(),
            display_name: String::new(),
            scopes: Permissions::BAN_MEMBERS.bits(),
        })
        .await
        .unwrap_err();
    assert_eq!(code(&escalation), i32::from(v1::ErrorCode::Forbidden));
    let lookalike = ada
        .create_bot(v1::CreateBot {
            username: "0wner".into(),
            display_name: String::new(),
            scopes: 0,
        })
        .await
        .unwrap_err();
    assert_eq!(code(&lookalike), i32::from(v1::ErrorCode::Invalid));

    // A bot never manages bots, whatever its scopes.
    let (_, token) = create_bot(&owner, "admin.bot", Permissions::ADMINISTRATOR).await;
    let bot = world.connect(Credentials::Bot(token)).await.unwrap();
    let refused = bot
        .create_bot(v1::CreateBot {
            username: "child".into(),
            display_name: String::new(),
            scopes: 0,
        })
        .await
        .unwrap_err();
    assert_eq!(code(&refused), i32::from(v1::ErrorCode::Forbidden));
    let listed = owner.list_bots(v1::ListBots {}).await.unwrap();
    assert_eq!(listed.bots.len(), 2);
    assert!(listed.bots.iter().all(|bot| bot.token.is_none()));
}

#[tokio::test]
async fn a_bot_cannot_join_voice() {
    let world = World::new().await;
    let owner = world.owner().await;
    // Even an administrator bot with every scope.
    let (bot, token) = create_bot(&owner, "deejay", Permissions::all()).await;
    owner
        .set_user_roles(v1::SetUserRoles {
            user_id: wire(bot),
            role_ids: vec![wire(db::ADMIN_ROLE_ID)],
        })
        .await
        .unwrap();
    let native = world.connect(Credentials::Bot(token)).await.unwrap();
    let voice = named(&native, "General", v1::ChannelKind::Voice);
    let refused = native
        .join_voice(v1::JoinVoice {
            channel_id: wire(voice),
            self_mute: false,
            self_deaf: false,
        })
        .await
        .unwrap_err();
    assert_eq!(code(&refused), i32::from(v1::ErrorCode::Forbidden));
    let held = native.state().read().permissions(voice).unwrap();
    assert!(!held.intersects(Permissions::CONNECT | Permissions::SPEAK | Permissions::STREAM));
    assert!(held.contains(Permissions::MANAGE_CHANNELS));
    assert_eq!(world.server.voice_members(), 0);
}

#[tokio::test]
async fn the_stream_never_shows_a_bot_a_channel_it_cannot_view() {
    let world = World::new().await;
    let owner = world.owner().await;
    let (_, token) = create_bot(&owner, "watcher", CHAT).await;
    let secret = owner
        .create_channel(v1::CreateChannel {
            kind: i32::from(v1::ChannelKind::Text),
            name: "secret".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    owner
        .set_channel_override(v1::SetChannelOverride {
            channel_id: wire(secret),
            role_id: wire(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: Permissions::VIEW_CHANNEL.bits(),
        })
        .await
        .unwrap();
    let mut stream = world.events(&token, "").await;
    let welcome = stream.next_of("welcome").await;
    let names: Vec<&str> = welcome["snapshot"]["channels"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|channel| channel["name"].as_str())
        .collect();
    assert!(
        names.contains(&"general") && !names.contains(&"secret"),
        "{names:?}"
    );

    for (channel, content) in [(secret, "hidden"), (general(&owner), "visible")] {
        let _sent = owner
            .send_message(v1::SendMessage {
                channel_id: wire(channel),
                content: content.into(),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let created = stream.next_of("messageCreated").await;
    assert_eq!(created["message"]["content"], "visible");

    // Granting view announces the channel before anything in it.
    owner
        .set_channel_override(v1::SetChannelOverride {
            channel_id: wire(secret),
            role_id: wire(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: 0,
        })
        .await
        .unwrap();
    let announced = stream.next_of("channelCreated").await;
    assert_eq!(announced["channel"]["name"], "secret");
}

#[tokio::test]
async fn rest_actions_and_reads_share_the_protobuf_json_schema() {
    let world = World::new().await;
    let owner = world.owner().await;
    let ada = world.member(&owner, "ada").await;
    let (bot, token) = create_bot(&owner, "helper", CHAT).await;
    let auth = format!("Bot {token}");
    let channel = general(&owner);
    let mut stream = world.events(&token, "?prefix=!").await;
    stream.next_of("welcome").await;

    // The prefix filter drops other messages; the bot sees its own posts.
    for content in ["no prefix", "!ping"] {
        let _sent = owner
            .send_message(v1::SendMessage {
                channel_id: wire(channel),
                content: content.into(),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let created = stream.next_of("messageCreated").await;
    assert_eq!(created["message"]["content"], "!ping");

    let (status, sent) = world
        .rest(
            "POST",
            &format!("/api/v1/channels/{channel}/messages"),
            &auth,
            Some(json!({ "content": "!pong", "nonce": "n-1" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    let own = stream.next_of("messageCreated").await;
    assert_eq!(own["message"]["id"], sent["messageId"]);
    assert_eq!(own["message"]["author"]["kind"], "USER_KIND_BOT");
    let message = id_from_bytes(&base64_decode(sent["messageId"].as_str().unwrap())).unwrap();
    let (status, _) = world
        .rest(
            "PATCH",
            &format!("/api/v1/messages/{message}"),
            &auth,
            Some(json!({ "content": "!pong (edited)" })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = world
        .rest(
            "DELETE",
            &format!("/api/v1/messages/{message}"),
            &auth,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let ada_id = ada.state().read().own_user.as_ref().unwrap().id.clone();
    let (status, dm) = world
        .rest(
            "POST",
            "/api/v1/dms",
            &auth,
            Some(json!({ "userIds": [base64_encode(&ada_id)] })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{dm}");
    let dm_id = dm["dmChannel"]["id"]
        .as_str()
        .unwrap()
        .replace('+', "-")
        .replace('/', "_");
    let (status, _) = world
        .rest(
            "POST",
            &format!("/api/v1/channels/{dm_id}/messages"),
            &auth,
            Some(post("hello in private")),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let dm_channel =
        id_from_bytes(&base64_decode(dm["dmChannel"]["id"].as_str().unwrap())).unwrap();
    assert_eq!(
        message_from(&ada, bot, dm_channel).await.content,
        "hello in private"
    );
}

#[tokio::test]
async fn rest_reads_page_members_and_list_visible_channels() {
    let world = World::new().await;
    let owner = world.owner().await;
    let (bot, token) = create_bot(&owner, "helper", CHAT).await;
    let auth = format!("Bot {token}");
    let (status, members) = world.rest("GET", "/api/v1/members", &auth, None).await;
    assert_eq!(status, StatusCode::OK);
    let kinds: Vec<(&str, &str)> = members["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["user"]["username"].as_str().unwrap(),
                m["user"]["kind"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(kinds.contains(&("helper", "USER_KIND_BOT")), "{kinds:?}");
    assert!(kinds.contains(&("owner", "USER_KIND_USER")), "{kinds:?}");
    let (status, page) = world
        .rest("GET", "/api/v1/members?limit=1", &auth, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["members"].as_array().unwrap().len(), 1);
    assert!(page["next"].is_string());

    let (status, channels) = world.rest("GET", "/api/v1/channels", &auth, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        channels["channels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "general" && c["effectivePermissions"].is_string())
    );
    assert_eq!(channels["directMessages"], json!([]));

    // Members see the bot with its kind in their member list.
    let listed = owner.state().read().members.get(&bot).cloned().unwrap();
    assert_eq!(listed.user.unwrap().kind, i32::from(v1::UserKind::Bot));
}

#[tokio::test]
async fn a_kick_or_ban_ends_a_bots_streams_at_once() {
    let world = World::new().await;
    let owner = world.owner().await;
    let (bot, token) = create_bot(&owner, "unruly", CHAT).await;

    let mut stream = world.events(&token, "").await;
    stream.next_of("welcome").await;
    owner
        .kick_user(v1::KickUser {
            user_id: wire(bot),
            reason: "calm down".into(),
        })
        .await
        .unwrap();
    assert_eq!(stream.next_of("kicked").await["reason"], "calm down");
    assert!(
        stream.next().await.is_none(),
        "the stream outlived the kick"
    );

    // A kick is only a disconnect; a ban refuses everything at once.
    let mut stream = world.events(&token, "").await;
    stream.next_of("welcome").await;
    let native = world
        .connect(Credentials::Bot(token.clone()))
        .await
        .unwrap();
    let mut native_events = native.subscribe();
    owner
        .ban_user(v1::BanUser {
            user_id: wire(bot),
            reason: "spam".into(),
            expires_at: None,
        })
        .await
        .unwrap();
    stream.next_of("banned").await;
    assert!(stream.next().await.is_none(), "the stream outlived the ban");
    timeout(WAIT, async {
        while !matches!(
            native_events.recv().await,
            Ok(ClientEvent::Removed { banned: true, .. })
        ) {}
    })
    .await
    .expect("the banned bot stayed connected");
    let (status, _) = world
        .rest("GET", "/api/v1/events", &format!("Bot {token}"), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn losing_a_role_hides_a_channel_from_the_stream_at_once() {
    let world = World::new().await;
    let owner = world.owner().await;
    let (bot, token) = create_bot(&owner, "lurker", CHAT).await;
    let lounge = owner
        .create_channel(v1::CreateChannel {
            kind: i32::from(v1::ChannelKind::Text),
            name: "lounge".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let regulars = owner
        .create_role(v1::CreateRole {
            name: "Regulars".into(),
            color: None,
            permissions: 0,
            position: 2,
        })
        .await
        .unwrap();
    for (role, allow, deny) in [
        (
            wire(db::EVERYONE_ROLE_ID),
            0,
            Permissions::VIEW_CHANNEL.bits(),
        ),
        (regulars.id.clone(), Permissions::VIEW_CHANNEL.bits(), 0),
    ] {
        owner
            .set_channel_override(v1::SetChannelOverride {
                channel_id: wire(lounge),
                role_id: role,
                allow,
                deny,
            })
            .await
            .unwrap();
    }
    owner
        .set_user_roles(v1::SetUserRoles {
            user_id: wire(bot),
            role_ids: vec![regulars.id.clone()],
        })
        .await
        .unwrap();
    let mut stream = world.events(&token, "").await;
    let welcome = stream.next_of("welcome").await;
    assert!(
        welcome["snapshot"]["channels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|channel| channel["name"] == "lounge")
    );

    owner
        .set_user_roles(v1::SetUserRoles {
            user_id: wire(bot),
            role_ids: Vec::new(),
        })
        .await
        .unwrap();
    for (channel, content) in [(lounge, "after the demotion"), (general(&owner), "public")] {
        let _sent = owner
            .send_message(v1::SendMessage {
                channel_id: wire(channel),
                content: content.into(),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let removed = stream.next_of("channelDeleted").await;
    assert_eq!(removed["channel"]["name"], "lounge");
    let created = stream.next_of("messageCreated").await;
    assert_eq!(created["message"]["content"], "public");
}

#[tokio::test]
async fn session_commands_are_refused_for_a_bot() {
    let world = World::new().await;
    let owner = world.owner().await;
    let (_, token) = create_bot(&owner, "sessionless", CHAT).await;
    let native = world.connect(Credentials::Bot(token)).await.unwrap();
    for msg in [
        v1::client_envelope::Msg::Logout(v1::Logout {}),
        v1::client_envelope::Msg::ListSessions(v1::ListSessions {}),
        v1::client_envelope::Msg::RevokeSession(v1::RevokeSession {
            session_id: wire(Id::generate()),
        }),
    ] {
        let refused = native.command(msg).await.unwrap_err();
        assert_eq!(code(&refused), i32::from(v1::ErrorCode::Forbidden));
        assert!(refused.to_string().contains("RevokeBotToken"), "{refused}");
    }
    // Still connected and signed in.
    native
        .typing(v1::Typing {
            channel_id: wire(general(&owner)),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn per_bot_limits_apply_across_transports() {
    let world = World::new().await;
    let owner = world.owner().await;
    let (_, token) = create_bot(&owner, "chatty", CHAT).await;
    let auth = format!("Bot {token}");
    let native = world
        .connect(Credentials::Bot(token.clone()))
        .await
        .unwrap();
    let mut limited = None;
    for attempt in 0..4 * ss_server::limits::BOT_REQUEST_BURST {
        let (status, _) = world.rest("GET", "/api/v1/channels", &auth, None).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(attempt);
            break;
        }
        assert_eq!(status, StatusCode::OK);
    }
    assert!(
        limited.is_some_and(|attempt| attempt >= ss_server::limits::BOT_REQUEST_BURST),
        "{limited:?}"
    );
    // The same budget covers QUIC commands.
    let refused = native
        .typing(v1::Typing {
            channel_id: wire(general(&owner)),
        })
        .await
        .unwrap_err();
    assert_eq!(code(&refused), i32::from(v1::ErrorCode::RateLimited));
    // Members are not affected.
    owner
        .typing(v1::Typing {
            channel_id: wire(general(&owner)),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_bot_holds_at_most_two_event_streams() {
    let world = World::new().await;
    let owner = world.owner().await;
    let (_, token) = create_bot(&owner, "streams", CHAT).await;
    let first = world.events(&token, "").await;
    let _second = world.events(&token, "").await;
    let (status, _) = world
        .rest("GET", "/api/v1/events", &format!("Bot {token}"), None)
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    drop(first);
    // The slot frees when the stream's task notices the client left.
    timeout(WAIT, async {
        loop {
            let response = world
                .server
                .public_routes()
                .oneshot(
                    Request::builder()
                        .uri("/api/v1/events")
                        .header(header::AUTHORIZATION, format!("Bot {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if response.status() == StatusCode::OK {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("a closed stream kept its slot");
}

#[tokio::test]
async fn the_rust_example_answers_over_quic() {
    let world = World::new().await;
    let owner = world.owner().await;
    let (bot, token) = create_bot(&owner, "dice", CHAT).await;
    let client = world.connect(Credentials::Bot(token)).await.unwrap();
    let task = tokio::spawn(async move { sideband_bot_example::run(&client).await });
    let channel = general(&owner);
    let _sent = owner
        .send_message(v1::SendMessage {
            channel_id: wire(channel),
            content: "!roll 2d6".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let answer = message_from(&owner, bot, channel).await;
    assert!(answer.content.starts_with("2d6: "), "{}", answer.content);
    task.abort();
}

#[tokio::test]
async fn the_python_example_answers_over_https() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("python3 is not installed; skipping the Python example");
        return;
    }
    let world = World::new().await;
    let owner = world.owner().await;
    let (bot, token) = create_bot(&owner, "echo", CHAT).await;
    let (port, https) = https(&world).await;
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/bot-python/bot.py"
    );
    let mut child = std::process::Command::new("python3")
        .arg("-I")
        .arg(script)
        .env("SIDEBAND_HOST", "127.0.0.1")
        .env("SIDEBAND_PORT", port.to_string())
        .env("SIDEBAND_BOT_TOKEN", &token)
        .env("SIDEBAND_FINGERPRINT", &world.fingerprint)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Wait until it has its stream, so the command is not sent before it.
    let stdout = child.stdout.take().unwrap();
    let line = timeout(
        WAIT,
        tokio::task::spawn_blocking(move || {
            let mut line = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut line)
                .map(|_| line)
        }),
    )
    .await
    .expect("the Python bot never signed in")
    .unwrap()
    .unwrap();
    let _killer = KillOnDrop(child);
    assert!(line.starts_with("signed in as echo"), "{line}");
    let channel = general(&owner);
    let _sent = owner
        .send_message(v1::SendMessage {
            channel_id: wire(channel),
            content: "!echo hello from python".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let answer = message_from(&owner, bot, channel).await;
    assert_eq!(answer.content, "hello from python");
    https.abort();
}

struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The public router on a TLS listener with the world's certificate.
async fn https(world: &World) -> (u16, JoinHandle<()>) {
    let identity = tls::generate_self_signed(world.dir.path()).await.unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(identity.certificates, identity.private_key)
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = ss_server::http::hardening::harden(world.server.public_routes());
    let task = tokio::spawn(ss_server::http::hardening::serve_tls(
        listener,
        app,
        Arc::new(config),
        std::future::pending(),
    ));
    (port, task)
}

fn base64_decode(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap()
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
