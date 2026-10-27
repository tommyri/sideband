//! Mock-server checks for fetch, cache, and suppression.

use std::{
    net::IpAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use ss_core::Id;
use ss_proto::v1::server_envelope;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};

use crate::{
    db::{self, NewMessage, PreviewStatus, link_previews, messages},
    events::EventBus,
};

use super::{
    PreviewJob,
    fetch::{Fetcher, Lookup},
    worker::enrich,
};

const OG: &[u8] = br#"<html><head>
<meta property="og:title" content="Hello">
<meta property="og:description" content="A page">
<meta property="og:image" content="https://cdn.example/a.png">
<meta property="og:site_name" content="Example">
</head></html>"#;

struct Mock {
    hits: Arc<AtomicUsize>,
    agent: Arc<Mutex<String>>,
    port: u16,
}

async fn mock() -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(Mutex::new(String::new()));
    let hits_task = Arc::clone(&hits);
    let agent_task = Arc::clone(&agent);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            hits_task.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0_u8; 2048];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(buf.get(..n).unwrap_or(&[]));
            remember_agent(&request, &agent_task);
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let (status, kind, extra, body) = routed(path, port);
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&body).await;
        }
    });
    Mock { hits, agent, port }
}

fn routed(path: &str, port: u16) -> (u16, &'static str, String, Vec<u8>) {
    match path {
        "/og" => (200, "text/html; charset=utf-8", String::new(), OG.to_vec()),
        "/go" => (
            302,
            "text/plain",
            "Location: http://10.1.2.3/secret\r\n".into(),
            Vec::new(),
        ),
        "/huge" => (200, "text/html", String::new(), huge()),
        "/embed" => (200, "text/html", String::new(), embed(port)),
        "/oembed.json" => (
            200,
            "application/json",
            String::new(),
            br#"{"title":"Clip","provider_name":"Tube","thumbnail_url":"https://i.example/t.jpg"}"#
                .to_vec(),
        ),
        _ => (404, "text/plain", String::new(), Vec::new()),
    }
}

fn huge() -> Vec<u8> {
    let mut body = b"<html><head><title>early</title>".to_vec();
    body.resize(1024 * 1024 + 64, b' ');
    body.extend_from_slice(br#"<meta property="og:description" content="late"></head></html>"#);
    body
}

fn embed(port: u16) -> Vec<u8> {
    format!(
        r#"<html><head><title>Page</title>
        <link rel="alternate" type="application/json+oembed" href="http://preview.test:{port}/oembed.json">
        </head></html>"#
    )
    .into_bytes()
}

fn remember_agent(request: &str, agent: &Mutex<String>) {
    for line in request.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("user-agent") {
            *agent.lock().expect("agent") = value.trim().to_owned();
        }
    }
}

fn fetcher(allow_loopback: bool, deny: &[&str]) -> Fetcher {
    let mut map = std::collections::HashMap::new();
    map.insert("preview.test".into(), vec![IpAddr::from([127, 0, 0, 1])]);
    Fetcher::new(
        Lookup::Fixed(map),
        allow_loopback,
        Vec::new(),
        deny.iter().map(|host| (*host).to_owned()).collect(),
    )
}

fn page(port: u16, path: &str) -> String {
    format!("http://preview.test:{port}{path}")
}

struct World {
    pool: db::TestPool,
    channel: Id,
    author: Id,
}

impl World {
    async fn new() -> Self {
        let pool = db::testing::pool().await;
        let mut conn = pool.write().await;
        let user = db::testing::user(&mut conn, 1).await;
        let channel = db::testing::text_channel(&mut conn, 2, user.id).await;
        let author = user.id;
        let channel = channel.id;
        drop(conn);
        Self {
            pool,
            channel,
            author,
        }
    }

    async fn say(&self, content: &str) -> Id {
        let mut conn = self.pool.write().await;
        let stored = messages::insert(
            &mut conn,
            NewMessage {
                id: Id::generate(),
                channel_id: self.channel,
                author_id: Some(self.author),
                webhook_id: None,
                content: content.into(),
                reply_to: None,
                mentions: Vec::new(),
                mentions_everyone: false,
            },
        )
        .await
        .unwrap();
        stored.id
    }
}

async fn title_of(
    events: &mut tokio::sync::broadcast::Receiver<crate::events::ServerEvent>,
) -> String {
    let event = timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    let server_envelope::Msg::MessageUpdated(updated) = event.msg.as_ref() else {
        panic!("expected MessageUpdated");
    };
    updated
        .message
        .as_ref()
        .unwrap()
        .link_previews
        .first()
        .and_then(|preview| preview.title.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn open_graph_is_cached_and_angle_brackets_suppress_it() {
    let server = mock().await;
    let url = page(server.port, "/og");
    let world = World::new().await;
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let client = fetcher(true, &[]);
    let first = world.say(&format!("see {url}")).await;
    enrich(
        &world.pool,
        &bus,
        &client,
        &job(first, &format!("see {url}")),
    )
    .await
    .unwrap();
    assert_eq!(title_of(&mut events).await, "Hello");
    assert!(server.agent.lock().expect("agent").contains("SocialSpeak/"));
    let second = world.say(&format!("again {url}")).await;
    enrich(
        &world.pool,
        &bus,
        &client,
        &job(second, &format!("again {url}")),
    )
    .await
    .unwrap();
    assert_eq!(title_of(&mut events).await, "Hello");
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);

    let hidden = format!("<{}>", page(server.port, "/og"));
    let hidden_id = world.say(&hidden).await;
    enrich(&world.pool, &bus, &client, &job(hidden_id, &hidden))
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_millis(200), events.recv())
            .await
            .is_err()
    );
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn redirect_to_a_private_address_and_loopback_dns_are_rejected() {
    let server = mock().await;
    let url = page(server.port, "/go");
    let world = World::new().await;
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let id = world.say(&url).await;
    enrich(&world.pool, &bus, &fetcher(true, &[]), &job(id, &url))
        .await
        .unwrap();
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        stored_status(&world.pool, &url).await,
        PreviewStatus::Failed
    );
    assert!(
        timeout(Duration::from_millis(200), events.recv())
            .await
            .is_err()
    );

    let (listener, port) = listen().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_task = Arc::clone(&hits);
    tokio::spawn(async move {
        loop {
            if listener.accept().await.is_ok() {
                hits_task.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    let mut map = std::collections::HashMap::new();
    map.insert("blocked.test".into(), vec![IpAddr::from([127, 0, 0, 1])]);
    let blocked = Fetcher::new(Lookup::Fixed(map), false, Vec::new(), Vec::new());
    let target = format!("http://blocked.test:{port}/");
    let blocked_id = world.say(&target).await;
    enrich(&world.pool, &bus, &blocked, &job(blocked_id, &target))
        .await
        .unwrap();
    assert_eq!(
        stored_status(&world.pool, &target).await,
        PreviewStatus::Failed
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn oversized_bodies_are_truncated_and_oembed_is_fetched() {
    let server = mock().await;
    let url = page(server.port, "/huge");
    let world = World::new().await;
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let client = fetcher(true, &[]);
    let id = world.say(&url).await;
    enrich(&world.pool, &bus, &client, &job(id, &url))
        .await
        .unwrap();
    let event = timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    let card = first_preview(&event);
    assert_eq!(card.title.as_deref(), Some("early"));
    assert_eq!(card.description, None);

    let embed = page(server.port, "/embed");
    let embed_id = world.say(&embed).await;
    enrich(&world.pool, &bus, &client, &job(embed_id, &embed))
        .await
        .unwrap();
    assert_eq!(title_of(&mut events).await, "Clip");
    assert_eq!(server.hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_denied_host_is_not_contacted() {
    let server = mock().await;
    let url = page(server.port, "/og");
    let world = World::new().await;
    let id = world.say(&url).await;
    enrich(
        &world.pool,
        &EventBus::new(),
        &fetcher(true, &["preview.test"]),
        &job(id, &url),
    )
    .await
    .unwrap();
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
    assert_eq!(
        stored_status(&world.pool, &url).await,
        PreviewStatus::Failed
    );
}

fn job(message_id: Id, content: &str) -> PreviewJob {
    PreviewJob {
        message_id,
        content: content.to_owned(),
    }
}

fn first_preview(event: &crate::events::ServerEvent) -> ss_proto::v1::LinkPreview {
    let server_envelope::Msg::MessageUpdated(updated) = event.msg.as_ref() else {
        panic!("expected MessageUpdated");
    };
    updated
        .message
        .as_ref()
        .unwrap()
        .link_previews
        .first()
        .unwrap()
        .clone()
}

async fn stored_status(pool: &db::Pool, url: &str) -> PreviewStatus {
    let mut conn = pool.read().await.unwrap();
    link_previews::get(&mut conn, url)
        .await
        .unwrap()
        .unwrap()
        .status
}

async fn listen() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}
