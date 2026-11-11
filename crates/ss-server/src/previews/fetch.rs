//! Preview HTTP client.
//!
//! DNS answers are filtered before the socket opens, and the connection is
//! pinned to one permitted address. Proxy variables (`HTTP_PROXY` and the
//! like) are ignored, since a proxy would resolve the name itself. Redirects
//! are followed by hand so each hop is checked again. The body stops at 1 MiB.
//! At most one fetch per host runs at a time; the gate forgets a host when
//! nobody is fetching it.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use parking_lot::Mutex;
use reqwest::{Client, StatusCode, Url};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::guard::{host_allowed, is_loopback, is_public};

const MAX_BODY: usize = 1024 * 1024;
const MAX_REDIRECTS: u8 = 3;
const USER_AGENT: &str = concat!("Sideband/", env!("CARGO_PKG_VERSION"), " (link-preview)");

/// How hostnames become addresses. Tests supply a fixed map.
pub(crate) enum Lookup {
    /// The system resolver.
    System,
    /// Hostname to addresses, with no network. Tests only.
    #[cfg(test)]
    Fixed(HashMap<String, Vec<IpAddr>>),
}

/// One fetched document, after redirects.
pub(crate) struct Document {
    /// URL that returned the body.
    pub url: Url,
    /// Lossy UTF-8 body, at most 1 MiB.
    pub body: String,
    /// Whether the response said it was JSON.
    pub json: bool,
}

/// Shared client, address policy, and per-host gate.
pub(crate) struct Fetcher {
    lookup: Lookup,
    allow_loopback: bool,
    allow: Vec<String>,
    deny: Vec<String>,
    gate: HostGate,
}

impl Fetcher {
    /// `allow_loopback` exists for the local mock server. Settings cannot set it.
    pub(crate) fn new(
        lookup: Lookup,
        allow_loopback: bool,
        allow: Vec<String>,
        deny: Vec<String>,
    ) -> Self {
        Self {
            lookup,
            allow_loopback,
            allow,
            deny,
            gate: HostGate::default(),
        }
    }

    /// Fetches `start`, following at most three redirects before `deadline`.
    pub(crate) async fn load(
        &self,
        start: &str,
        accept: &str,
        deadline: Instant,
    ) -> Result<Document, ()> {
        let mut current = Url::parse(start).map_err(|_| ())?;
        for hop in 0..=MAX_REDIRECTS {
            let response = self.hop(&current, accept, deadline).await?;
            if is_redirect(response.status()) {
                if hop == MAX_REDIRECTS {
                    return Err(());
                }
                current = next_url(&current, &response)?;
                continue;
            }
            if !response.status().is_success() {
                return Err(());
            }
            let json = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.to_ascii_lowercase().contains("json"));
            let body = read_capped(response).await?;
            return Ok(Document {
                url: current,
                body,
                json,
            });
        }
        Err(())
    }

    async fn hop(
        &self,
        url: &Url,
        accept: &str,
        deadline: Instant,
    ) -> Result<reqwest::Response, ()> {
        let host_raw = url.host_str().ok_or(())?.to_owned();
        let host = host_raw.to_ascii_lowercase();
        if !host_allowed(&host, &self.allow, &self.deny) || !url.username().is_empty() {
            return Err(());
        }
        let port = url.port_or_known_default().ok_or(())?;
        let ips = self.addresses(&host, port).await?;
        let Some(ip) = ips.into_iter().find(|ip| self.permits(*ip)) else {
            return Err(());
        };
        let permit = self.gate.enter(&host).await;
        let remaining = deadline.checked_duration_since(Instant::now()).ok_or(())?;
        ss_net::tls::install_default_crypto_provider();
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(remaining)
            .connect_timeout(remaining)
            .user_agent(USER_AGENT)
            .resolve(&host_raw, SocketAddr::new(ip, port))
            .build()
            .map_err(|_| ())?;
        let response = tokio::time::timeout(remaining, async {
            client
                .get(url.clone())
                .header(reqwest::header::ACCEPT, accept)
                .send()
                .await
                .map_err(|_| ())
        })
        .await
        .map_err(|_| ())??;
        drop(permit);
        Ok(response)
    }

    async fn addresses(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, ()> {
        if let Ok(ip) = host.parse() {
            return Ok(vec![ip]);
        }
        match &self.lookup {
            Lookup::System => system_lookup(host, port).await,
            #[cfg(test)]
            Lookup::Fixed(map) => map
                .get(host)
                .cloned()
                .filter(|ips| !ips.is_empty())
                .ok_or(()),
        }
    }

    fn permits(&self, ip: IpAddr) -> bool {
        is_public(ip) || (self.allow_loopback && is_loopback(ip))
    }
}

/// One fetch at a time per host. The map holds only hosts being fetched or
/// waited on, so it does not grow with every host ever previewed.
#[derive(Default)]
struct HostGate {
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl HostGate {
    async fn enter(&self, host: &str) -> HostPermit<'_> {
        let sem = {
            let mut hosts = self.hosts.lock();
            Arc::clone(
                hosts
                    .entry(host.to_owned())
                    .or_insert_with(|| Arc::new(Semaphore::new(1))),
            )
        };
        let mut held = HostPermit {
            gate: self,
            host: host.to_owned(),
            sem: Arc::clone(&sem),
            permit: None,
        };
        // The semaphore is never closed, so acquiring cannot fail.
        held.permit = sem.acquire_owned().await.ok();
        held
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.hosts.lock().len()
    }
}

/// The turn for one host. Dropping it lets the next fetch in, and removes the
/// host when no one else holds or awaits it.
struct HostPermit<'a> {
    gate: &'a HostGate,
    host: String,
    sem: Arc<Semaphore>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Drop for HostPermit<'_> {
    fn drop(&mut self) {
        drop(self.permit.take());
        let mut hosts = self.gate.hosts.lock();
        // Clones are only made under this lock, so two references (the map's
        // and ours) mean nobody else holds or waits for this host.
        let idle = hosts
            .get(&self.host)
            .is_some_and(|sem| Arc::ptr_eq(sem, &self.sem) && Arc::strong_count(sem) == 2);
        if idle {
            hosts.remove(&self.host);
        }
    }
}

async fn system_lookup(host: &str, port: u16) -> Result<Vec<IpAddr>, ()> {
    let mut ips = Vec::new();
    for addr in tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| ())?
    {
        if !ips.contains(&addr.ip()) {
            ips.push(addr.ip());
        }
    }
    if ips.is_empty() { Err(()) } else { Ok(ips) }
}

async fn read_capped(mut response: reqwest::Response) -> Result<String, ()> {
    let mut body = Vec::new();
    while body.len() < MAX_BODY {
        let Some(chunk) = response.chunk().await.map_err(|_| ())? else {
            break;
        };
        let room = MAX_BODY.saturating_sub(body.len());
        let take = room.min(chunk.len());
        body.extend(chunk.iter().take(take).copied());
        if take < chunk.len() {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn next_url(current: &Url, response: &reqwest::Response) -> Result<Url, ()> {
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(())?;
    let next = current.join(location).map_err(|_| ())?;
    if (next.scheme() == "http" || next.scheme() == "https") && next.username().is_empty() {
        Ok(next)
    } else {
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{Fetcher, HostGate, Lookup};

    const PROXY_CHILD: &str = "SS_PREVIEW_PROXY_CHILD";

    #[tokio::test]
    async fn the_host_gate_forgets_hosts_nobody_is_fetching() {
        let gate = HostGate::default();
        let held = gate.enter("a.example").await;
        for host in ["b.example", "c.example", "d.example"] {
            drop(gate.enter(host).await);
        }
        assert_eq!(gate.tracked(), 1, "only the held host remains");
        drop(held);
        assert_eq!(gate.tracked(), 0);
    }

    /// `HTTP_PROXY` and friends are read when the client is built, so the
    /// check runs this test again in a child process with them set.
    #[test]
    fn proxy_variables_do_not_route_preview_fetches() {
        if std::env::var_os(PROXY_CHILD).is_some() {
            fetch_directly();
            return;
        }
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        proxy.set_nonblocking(true).unwrap();
        let url = format!("http://{}", proxy.local_addr().unwrap());
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "previews::fetch::tests::proxy_variables_do_not_route_preview_fetches",
                "--test-threads=1",
            ])
            .env(PROXY_CHILD, "1")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy");
        for name in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            child.env(name, &url);
        }
        let output = child.output().unwrap();
        assert!(
            proxy.accept().is_err(),
            "the preview client connected to the proxy"
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    fn fetch_directly() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = target.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (mut socket, _) = target.accept().await.unwrap();
                let mut buf = [0_u8; 1024];
                let _ = socket.read(&mut buf).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ntarget",
                    )
                    .await;
            });
            let fetcher = Fetcher::new(Lookup::System, true, Vec::new(), Vec::new());
            let deadline = Instant::now() + Duration::from_secs(3);
            let document = fetcher
                .load(&format!("http://127.0.0.1:{port}/"), "text/html", deadline)
                .await
                .expect("direct fetch failed");
            assert_eq!(document.body, "target");
        });
    }
}
