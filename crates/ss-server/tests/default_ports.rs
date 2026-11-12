//! A server on the ports `ss-server init` writes and a client given only the
//! host name reach each other over QUIC and HTTPS.

use std::{
    io::{BufRead, BufReader},
    net::SocketAddr,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use ss_client_core::{Client, Credentials, ServerAddr, v1};
use ss_net::pins::{MemoryPinStore, PinStore};
use ss_proto::validate::{id_from_bytes, id_to_bytes};
use ss_server::config;

/// Kills the server when the test ends, pass or fail.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The test cannot bind 443, so it moves the QUIC port to a free one and
/// keeps the HTTPS port where `init` puts it relative to QUIC. The client is
/// given only the host and the QUIC port, as the desktop does for an address
/// typed without a port.
#[tokio::test]
async fn a_server_on_the_init_defaults_serves_history_where_the_client_derives_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap().to_owned();
    let created = succeed(&["init", "--data-dir", &path]);
    let invite = after(&created, "owner invite: ");
    let fingerprint = after(&created, "fingerprint: ");
    let file = config::path_in(dir.path());
    let written = std::fs::read_to_string(&file).unwrap();
    let defaults = config::parse(&written).unwrap().server;

    let quic_port = free_port();
    let http_port = like_init(quic_port, defaults.bind_quic, defaults.bind_http);
    let moved = written
        .replace(
            &format!("bind_quic = \"{}\"", defaults.bind_quic),
            &format!("bind_quic = \"127.0.0.1:{quic_port}\""),
        )
        .replace(
            &format!("bind_http = \"{}\"", defaults.bind_http),
            &format!("bind_http = \"127.0.0.1:{http_port}\""),
        );
    assert_ne!(moved, written, "the binds were not rewritten");
    std::fs::write(&file, moved).unwrap();
    let _server = start(&path);

    let address = ServerAddr::new("127.0.0.1", quic_port).unwrap();
    let pins = Arc::new(MemoryPinStore::default());
    pins.set(
        &address.id().unwrap(),
        ss_net::tls::Fingerprint::try_from(fingerprint).unwrap(),
    )
    .unwrap();
    let client = tokio::time::timeout(
        Duration::from_secs(10),
        Client::connect(
            address,
            Credentials::Register(v1::Register {
                invite_code: invite,
                username: "owner".into(),
                password: "test-password".into(),
            }),
            pins,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let general = text_channel(&client);
    let _sent = client
        .send_message(v1::SendMessage {
            channel_id: id_to_bytes(general),
            content: "over https".into(),
            nonce: Some("over https".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let page = client
        .load_older(general, None)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "history at the derived HTTPS port {quic_port} failed while the server \
             serves HTTPS on {http_port}: {error}"
            )
        });
    assert!(page.iter().any(|message| message.content == "over https"));

    let typed = ServerAddr::new(&defaults.public_host, ss_net::DEFAULT_PORT).unwrap();
    assert_eq!(typed.port, defaults.bind_quic.port());
    assert_eq!(typed.https_port, defaults.bind_http.port());
}

/// `quic_port` plus the distance `init` puts between the QUIC and HTTPS
/// ports (subtracted instead when adding would not fit in a port).
fn like_init(quic_port: u16, quic: SocketAddr, http: SocketAddr) -> u16 {
    let gap = i32::from(http.port()) - i32::from(quic.port());
    let port = i32::from(quic_port);
    u16::try_from(port + gap)
        .or_else(|_| u16::try_from(port - gap))
        .unwrap()
}

/// A loopback port free on both UDP and TCP right now.
fn free_port() -> u16 {
    loop {
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = udp.local_addr().unwrap().port();
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

fn start(path: &str) -> Running {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ss-server"))
        .args(["run", "--data-dir", path])
        .env("SS_INTERNAL_BIND", "127.0.0.1:0")
        .env_remove("SS_BIND_QUIC")
        .env_remove("SS_BIND_HTTP")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let running = Running(child);
    let mut reader = BufReader::new(stdout);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut line = String::new();
    while !line.contains("listening") {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0, "server exited");
        assert!(Instant::now() <= deadline, "server did not start");
    }
    std::thread::spawn(move || std::io::copy(&mut reader, &mut std::io::sink()));
    running
}

fn text_channel(client: &Client) -> ss_core::Id {
    let state = client.state();
    let state = state.read();
    let channel = state
        .channels
        .values()
        .find(|channel| channel.kind == i32::from(v1::ChannelKind::Text))
        .unwrap();
    id_from_bytes(&channel.id).unwrap()
}

fn succeed(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_ss-server"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn after(text: &str, prefix: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap()
        .trim()
        .to_owned()
}
