//! Link-preview jobs.
//!
//! [`PreviewQueue`] keeps two workers. A full queue drops the job. Each host
//! is fetched by one worker at a time. Settings `link_previews=off` disables
//! fetching; `link_preview_allow` and `link_preview_deny` are comma-separated
//! host lists (`*.example.com` matches a suffix).

mod extract;
mod fetch;
mod guard;
mod parse;
mod worker;

pub(crate) use extract::preview_urls;
pub(crate) use parse::{merge_oembed, parse_html};

use std::sync::Arc;

use ss_core::Id;
use tokio::sync::mpsc;

use crate::{
    db::{self, settings},
    events::EventBus,
};

use fetch::{Fetcher, Lookup};

/// One message whose body may contain URLs.
#[derive(Debug)]
pub struct PreviewJob {
    /// Stored message.
    pub message_id: Id,
    /// Raw markdown body.
    pub content: String,
}

/// How preview fetches are chosen. `allow_loopback` is for tests only.
pub(crate) struct PreviewConfig {
    /// When false, jobs are discarded.
    pub enabled: bool,
    /// Empty means every host that is not denied.
    pub allow: Vec<String>,
    /// Hosts that are never fetched.
    pub deny: Vec<String>,
    /// Lets a test mock answer on 127.0.0.1. Settings cannot turn this on.
    pub allow_loopback: bool,
    /// Resolver. Production uses the system resolver.
    pub lookup: Lookup,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow: Vec::new(),
            deny: Vec::new(),
            allow_loopback: false,
            lookup: Lookup::System,
        }
    }
}

/// Bounded queue. A full queue drops the job instead of waiting on a worker.
#[derive(Clone)]
pub struct PreviewQueue {
    tx: mpsc::Sender<PreviewJob>,
}

impl PreviewQueue {
    /// Loads settings, then starts two workers.
    #[must_use]
    #[allow(clippy::new_without_default)]
    pub fn new(pool: Arc<db::Pool>, bus: EventBus) -> Self {
        let (tx, rx) = mpsc::channel::<PreviewJob>(32);
        tokio::spawn(async move {
            let config = load_config(&pool).await;
            serve(rx, pool, bus, config).await;
        });
        Self { tx }
    }

    /// Queues `job` when there is room.
    pub fn enqueue(&self, job: PreviewJob) {
        let _ = self.tx.try_send(job);
    }
}

async fn serve(
    rx: mpsc::Receiver<PreviewJob>,
    pool: Arc<db::Pool>,
    bus: EventBus,
    config: PreviewConfig,
) {
    let enabled = config.enabled;
    let fetcher = Arc::new(Fetcher::new(
        config.lookup,
        config.allow_loopback,
        config.allow,
        config.deny,
    ));
    let incoming = Arc::new(tokio::sync::Mutex::new(rx));
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..2 {
        let incoming = Arc::clone(&incoming);
        let pool = Arc::clone(&pool);
        let bus = bus.clone();
        let fetcher = Arc::clone(&fetcher);
        workers.spawn(async move {
            loop {
                let job = {
                    let mut incoming = incoming.lock().await;
                    incoming.recv().await
                };
                let Some(job) = job else { break };
                if !enabled {
                    continue;
                }
                if let Err(error) = worker::enrich(&pool, &bus, &fetcher, &job).await {
                    tracing::debug!(%error, message_id = %job.message_id, "link preview failed");
                }
            }
        });
    }
    while workers.join_next().await.is_some() {}
}

async fn load_config(pool: &db::Pool) -> PreviewConfig {
    let mut config = PreviewConfig::default();
    let Ok(mut conn) = pool.read().await else {
        return config;
    };
    if let Ok(Some(setting)) = settings::get(&mut conn, "link_previews").await {
        config.enabled = setting.value != "off";
    }
    if let Ok(Some(setting)) = settings::get(&mut conn, "link_preview_allow").await {
        config.allow = split_hosts(&setting.value);
    }
    if let Ok(Some(setting)) = settings::get(&mut conn, "link_preview_deny").await {
        config.deny = split_hosts(&setting.value);
    }
    config
}

fn split_hosts(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(test)]
mod tests;
