//! Preview HTTP client.
//!
//! DNS answers are filtered before the socket opens, and the connection is
//! pinned to one permitted address. Redirects are followed by hand so each
//! hop is checked again. The body stops at 1 MiB.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use reqwest::{Client, StatusCode, Url};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::guard::{host_allowed, is_loopback, is_public};

const MAX_BODY: usize = 1024 * 1024;
const MAX_REDIRECTS: u8 = 3;
const USER_AGENT: &str = concat!("SocialSpeak/", env!("CARGO_PKG_VERSION"), " (link-preview)");

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
        let client = Client::builder()
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

#[derive(Default)]
struct HostGate {
    hosts: tokio::sync::Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl HostGate {
    async fn enter(&self, host: &str) -> OwnedSemaphorePermit {
        let sem = {
            let mut hosts = self.hosts.lock().await;
            hosts
                .entry(host.to_owned())
                .or_insert_with(|| Arc::new(Semaphore::new(1)))
                .clone()
        };
        sem.acquire_owned().await.expect("host gate")
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
