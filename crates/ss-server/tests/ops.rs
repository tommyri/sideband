//! Config, backup, metrics, and shutdown of the authenticated process.

use std::{
    io::{BufRead, BufReader, Write},
    net::SocketAddr,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use ss_net::{
    client,
    pins::{MemoryPinStore, PinStore, ServerId},
};
use ss_proto::v1::{self, client_envelope::Msg};

const PASSWORD: &str = "password-ok";

#[test]
fn init_prints_the_invite_and_check_config_rejects_a_typo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let created = succeed(&["init", "--data-dir", path]);
    assert!(created.contains("owner invite: "), "{created}");
    assert!(created.contains("fingerprint: "), "{created}");
    assert_eq!(
        succeed(&["check-config", "--data-dir", path]).trim(),
        "config ok"
    );

    let config = dir.path().join("config.toml");
    let text = std::fs::read_to_string(&config)
        .unwrap()
        .replace("name =", "nme =");
    std::fs::write(&config, text).unwrap();
    let failed = Command::new(env!("CARGO_BIN_EXE_ss-server"))
        .args(["check-config", "--data-dir", path])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("server.nme"), "{stderr}");
}

#[test]
fn backup_contains_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    succeed(&["init", "--data-dir", path]);
    let tar_path = dir.path().join("copy.tar");
    succeed(&["backup", tar_path.to_str().unwrap(), "--data-dir", path]);
    let file = std::fs::File::open(tar_path).unwrap();
    let mut archive = tar::Archive::new(file);
    let names: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().to_path_buf())
        .collect();
    assert!(
        names.iter().any(|name| name.ends_with("socialspeak.db")),
        "{names:?}"
    );
}

#[tokio::test]
async fn run_serves_metrics_and_shuts_down_with_a_client() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap().to_owned();
    let created = succeed(&["init", "--data-dir", &path]);
    let invite = created
        .lines()
        .find_map(|line| line.strip_prefix("owner invite: "))
        .unwrap()
        .trim()
        .to_owned();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ss-server"))
        .args(["run", "--data-dir", &path])
        .env("SS_BIND_QUIC", "127.0.0.1:0")
        .env("SS_BIND_HTTP", "127.0.0.1:0")
        .env("SS_INTERNAL_BIND", "127.0.0.1:0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let logged = std::thread::spawn(move || {
        let mut err = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut err);
        err
    });
    let line = read_line(&mut BufReader::new(stdout), "listening");
    let internal = addr_after(&line, "internal=");
    let quic = addr_after(&line, "quic=");
    let health = http_get(internal, "/healthz");
    assert!(health.contains("200"), "{health}");
    let metrics = http_get(internal, "/metrics");
    for series in [
        "socialspeak_connections",
        "socialspeak_voice_members",
        "socialspeak_datagrams_forwarded",
        "socialspeak_datagrams_dropped",
        "socialspeak_datagrams_received",
        "socialspeak_datagrams_rejected",
        "socialspeak_messages",
        "socialspeak_sessions",
        "socialspeak_slow_disconnects",
        "socialspeak_db_seconds",
    ] {
        assert!(metrics.contains(series), "{series} missing from {metrics}");
    }

    let fingerprint = created
        .lines()
        .find_map(|line| line.strip_prefix("fingerprint: "))
        .unwrap()
        .trim()
        .to_owned();
    let pins = std::sync::Arc::new(MemoryPinStore::default());
    pins.set(
        &ServerId::new("127.0.0.1", quic.port()).unwrap(),
        ss_net::tls::Fingerprint::try_from(fingerprint).unwrap(),
    )
    .unwrap();
    let mut connection = tokio::time::timeout(
        Duration::from_secs(5),
        client::connect("127.0.0.1", quic.port(), pins),
    )
    .await
    .unwrap()
    .unwrap();
    let mut stream = connection.open_control().await.unwrap();
    stream
        .send(&v1::ClientEnvelope {
            request_id: 1,
            msg: Some(Msg::Hello(v1::Hello {
                protocol_version: 1,
                client_name: "ops".into(),
                auth: Some(v1::hello::Auth::Register(v1::Register {
                    invite_code: invite,
                    username: "ada".into(),
                    password: PASSWORD.into(),
                })),
                ..Default::default()
            })),
        })
        .await
        .unwrap();
    let welcome = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(welcome.request_id == 1, "{welcome:?}");

    check_connections(&path, internal);

    let started = Instant::now();
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let finished = child.wait().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shutdown took {:?}",
        started.elapsed()
    );
    assert!(finished.success(), "{}", logged.join().unwrap());
}

fn succeed(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_ss-server"))
        .args(args)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?}\n{stderr}",
        output.status
    );
    String::from_utf8(output.stdout).unwrap()
}

fn read_line(reader: &mut impl BufRead, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "server exited before {needle}"
        );
        if line.contains(needle) {
            return line;
        }
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for {needle}, last line {line}"
        );
    }
}

fn addr_after(line: &str, key: &str) -> SocketAddr {
    let rest = line.split(key).nth(1).unwrap();
    let token = rest.split_whitespace().next().unwrap();
    token.parse().unwrap()
}

fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut body = String::new();
    let _ = std::io::Read::read_to_string(&mut stream, &mut body);
    body
}

fn check_connections(path: &str, internal: SocketAddr) {
    let inspected = Command::new(env!("CARGO_BIN_EXE_ss-server"))
        .args(["conn", "list", "--data-dir", path])
        .env("SS_INTERNAL_BIND", internal.to_string())
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["pathMtuBytes"].as_u64().unwrap() >= 1200);
    assert!(rows[0]["congestionWindowBytes"].as_u64().unwrap() > 0);
    let text = String::from_utf8(inspected.stdout).unwrap();
    for secret in [PASSWORD, "ada", "ops", "127.0.0.1"] {
        assert!(!text.contains(secret));
    }
}
