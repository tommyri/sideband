//! Incoming webhooks: create over QUIC, post over HTTP, fan out the message.

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use ss_net::{
    client,
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};
use ss_proto::{
    v1::{self, client_envelope::Msg, server_envelope::Msg as Event},
    validate::id_from_bytes,
};
use ss_server::{auth, connection, db};
use tokio::time::timeout;
use tower::ServiceExt;

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(5);

struct World {
    endpoint: Endpoint,
    port: u16,
    pins: Arc<MemoryPinStore>,
    server: Arc<connection::Server>,
    task: tokio::task::JoinHandle<()>,
    pool: Arc<db::Pool>,
    code: String,
    _dir: tempfile::TempDir,
}

struct Hook {
    id: String,
    token: String,
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
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(
            &ServerId::new("127.0.0.1", endpoint.local_addr().unwrap().port()).unwrap(),
            fingerprint,
        )
        .unwrap();
        let server = Arc::new(connection::Server::open(Arc::clone(&pool)).await.unwrap());
        let task = tokio::spawn(Arc::clone(&server).run(endpoint.clone()));
        Self {
            port: endpoint.local_addr().unwrap().port(),
            endpoint,
            pins,
            server,
            task,
            pool,
            code,
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
}

impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

#[tokio::test]
async fn a_webhook_post_fans_out_and_a_revoked_token_is_not_found() {
    let world = World::new().await;
    let (_ada_conn, mut ada) = world.connect().await;
    let welcome = hello(&mut ada, register_owner(&world, "ada").await).await;
    assert!(
        welcome
            .server_info
            .unwrap()
            .features
            .iter()
            .any(|feature| feature == "webhooks")
    );
    let channel = text_named(welcome.snapshot.as_ref().unwrap(), "general");

    let missing = command(
        &mut ada,
        2,
        Msg::CreateWebhook(v1::CreateWebhook {
            channel_id: channel.clone(),
            name: "   ".into(),
        }),
    )
    .await;
    assert!(error_text(&missing).contains("1 to 80"));

    let created = ack_webhook(
        &command(
            &mut ada,
            3,
            Msg::CreateWebhook(v1::CreateWebhook {
                channel_id: channel.clone(),
                name: "ci".into(),
            }),
        )
        .await,
    );
    let hook = Hook {
        id: id_from_bytes(&created.id).unwrap().to_string(),
        token: created.token.clone().unwrap(),
    };
    assert_eq!(created.name, "ci");

    let posted = post(
        &world,
        &hook,
        &json!({"content": "shipped", "username": "ada-bot"}),
    )
    .await;
    assert_eq!(posted.0, StatusCode::OK);
    let message = next_message(&mut ada).await;
    assert!(message.author.is_none());
    assert_eq!(message.webhook_name.as_deref(), Some("ada-bot"));
    assert_eq!(message.content, "shipped");
    assert_eq!(
        id_from_bytes(message.webhook_id.as_ref().unwrap())
            .unwrap()
            .to_string(),
        hook.id
    );

    expect_readable(&world, &mut ada, &hook).await;

    let wrong = post(
        &world,
        &Hook {
            id: hook.id.clone(),
            token: "not-the-token".into(),
        },
        &json!({"content": "nope"}),
    )
    .await;
    assert_eq!(wrong.0, StatusCode::NOT_FOUND);

    let empty = post(&world, &hook, &json!({"content": "  "})).await;
    assert_eq!(empty.0, StatusCode::BAD_REQUEST);

    command(
        &mut ada,
        4,
        Msg::DeleteWebhook(v1::DeleteWebhook {
            webhook_id: created.id.clone(),
        }),
    )
    .await;
    let revoked = post(&world, &hook, &json!({"content": "after"})).await;
    assert_eq!(revoked.0, StatusCode::NOT_FOUND);
    let actions = audit_actions(&world).await;
    assert!(actions.iter().any(|action| action == "webhook.create"));
    assert!(actions.iter().any(|action| action == "webhook.delete"));
}

async fn audit_actions(world: &World) -> Vec<String> {
    let mut conn = world.pool.write().await;
    db::audit_log::recent(&mut conn, 20)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.action)
        .collect()
}

#[tokio::test]
async fn the_thirty_first_post_is_rate_limited() {
    let world = World::new().await;
    let (_ada_conn, mut ada) = world.connect().await;
    let welcome = hello(&mut ada, register_owner(&world, "ada").await).await;
    let channel = text_named(welcome.snapshot.as_ref().unwrap(), "general");
    let created = ack_webhook(
        &command(
            &mut ada,
            2,
            Msg::CreateWebhook(v1::CreateWebhook {
                channel_id: channel,
                name: "alerts".into(),
            }),
        )
        .await,
    );
    let hook = Hook {
        id: id_from_bytes(&created.id).unwrap().to_string(),
        token: created.token.unwrap(),
    };
    for _ in 0..30 {
        let (status, _) = post(&world, &hook, &json!({"content": "tick"})).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _) = post(&world, &hook, &json!({"content": "tick"})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

async fn expect_readable(world: &World, stream: &mut ss_net::control::ClientStream, hook: &Hook) {
    let github = post(
        world,
        hook,
        &json!({
            "ref": "refs/heads/main",
            "repository": {"full_name": "acme/socialspeak"},
            "pusher": {"name": "ada"},
            "commits": [{"message": "fix the login form"}]
        }),
    )
    .await;
    assert_eq!(github.0, StatusCode::OK);
    let github_message = next_message(stream).await;
    assert!(
        github_message.content.contains("acme/socialspeak"),
        "{}",
        github_message.content
    );
    assert!(github_message.content.contains("fix the login form"));
    assert_eq!(github_message.webhook_name.as_deref(), Some("ci"));

    let grafana = post(
        world,
        hook,
        &json!({"title": "CPU high", "message": "host-1 is at 99%", "state": "alerting"}),
    )
    .await;
    assert_eq!(grafana.0, StatusCode::OK);
    let grafana_message = next_message(stream).await;
    assert!(grafana_message.content.contains("CPU high"));
    assert!(grafana_message.content.contains("99%"));

    let embed = post(
        world,
        hook,
        &json!({"embeds": [{"title": "[FIRING] CPU", "description": "host-1 is at 99%"}]}),
    )
    .await;
    assert_eq!(embed.0, StatusCode::OK);
    let embed_message = next_message(stream).await;
    assert!(embed_message.content.contains("[FIRING] CPU"));
}

async fn register_owner(world: &World, username: &str) -> v1::Hello {
    auth::Auth::new(Arc::clone(&world.pool))
        .hello(
            &register(&world.code, username),
            "203.0.113.9".parse().unwrap(),
        )
        .await
        .unwrap();
    login_hello(username)
}

async fn hello(stream: &mut ss_net::control::ClientStream, hello: v1::Hello) -> v1::Welcome {
    match command(stream, 1, Msg::Hello(hello)).await.msg {
        Some(Event::Welcome(welcome)) => welcome,
        other => panic!("expected Welcome, got {other:?}"),
    }
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
        let frame = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
        if frame.request_id == request_id {
            return frame;
        }
    }
}

async fn next_message(stream: &mut ss_net::control::ClientStream) -> v1::Message {
    loop {
        let frame = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
        if let Some(Event::MessageCreated(created)) = frame.msg {
            return created.message.unwrap();
        }
    }
}

async fn post(world: &World, hook: &Hook, body: &Value) -> (StatusCode, Value) {
    let response = world
        .server
        .webhook_routes()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/webhooks/{}/{}", hook.id, hook.token))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

fn text_named(snapshot: &v1::Snapshot, name: &str) -> bytes::Bytes {
    snapshot
        .channels
        .iter()
        .find(|channel| channel.name == name && channel.kind == i32::from(v1::ChannelKind::Text))
        .unwrap_or_else(|| panic!("missing text channel {name}"))
        .id
        .clone()
}

fn ack_webhook(envelope: &v1::ServerEnvelope) -> v1::Webhook {
    match &envelope.msg {
        Some(Event::Ack(ack)) => match &ack.result {
            Some(v1::ack::Result::Webhook(webhook)) => webhook.clone(),
            other => panic!("expected a webhook, got {other:?}"),
        },
        other => panic!("expected Ack, got {other:?}"),
    }
}

fn error_text(envelope: &v1::ServerEnvelope) -> String {
    match &envelope.msg {
        Some(Event::Error(error)) => error.message.clone(),
        other => panic!("expected Error, got {other:?}"),
    }
}

fn login_hello(username: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: "desk".into(),
        auth: Some(v1::hello::Auth::Login(v1::Login {
            username: username.into(),
            password: PASSWORD.into(),
        })),
        ..Default::default()
    }
}

fn register(code: &str, username: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: "test".into(),
        auth: Some(v1::hello::Auth::Register(v1::Register {
            invite_code: code.into(),
            username: username.into(),
            password: PASSWORD.into(),
        })),
        ..Default::default()
    }
}
