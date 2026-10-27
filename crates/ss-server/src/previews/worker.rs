//! Turns a queued message into cached preview cards.
//!
//! A fresh URL is fetched once. The same URL is reused until its TTL, 24
//! hours after success and 1 hour after failure. Viewers then get
//! `MessageUpdated`.

use std::time::{Duration, Instant};

use ss_core::Timestamp;
use ss_proto::v1::{self, server_envelope};

use crate::{
    db::{self, NewLinkPreview, PreviewStatus, link_previews, messages},
    events::EventBus,
};

use super::{
    PreviewJob,
    extract::preview_urls,
    fetch::{Document, Fetcher},
    parse::{self, Card},
};

const DAY: Duration = Duration::from_hours(24);
const HOUR: Duration = Duration::from_hours(1);

/// Fetches, caches, and publishes previews for `job`.
pub(crate) async fn enrich(
    pool: &db::Pool,
    bus: &EventBus,
    fetcher: &Fetcher,
    job: &PreviewJob,
) -> Result<(), db::Error> {
    let urls = preview_urls(&job.content);
    if urls.is_empty() {
        return Ok(());
    }
    let mut ready = false;
    for (position, url) in urls.iter().enumerate() {
        let preview = resolve(pool, fetcher, url).await?;
        if preview.status == PreviewStatus::Ready {
            let mut conn = pool.write().await;
            link_previews::link(
                &mut conn,
                job.message_id,
                url,
                i64::try_from(position).unwrap_or(i64::MAX),
            )
            .await?;
            ready = true;
        }
    }
    if ready {
        publish(pool, bus, job.message_id).await?;
    }
    Ok(())
}

async fn resolve(
    pool: &db::Pool,
    fetcher: &Fetcher,
    url: &str,
) -> Result<db::LinkPreview, db::Error> {
    if let Some(row) = fresh(pool, url).await? {
        return Ok(row);
    }
    let preview = fetch_card(fetcher, url)
        .await
        .map_or_else(|| failed(url), |card| stored(url, card));
    let mut conn = pool.write().await;
    link_previews::upsert(&mut conn, preview).await
}

async fn fresh(pool: &db::Pool, url: &str) -> Result<Option<db::LinkPreview>, db::Error> {
    let mut conn = pool.read().await?;
    let Some(row) = link_previews::get(&mut conn, url).await? else {
        return Ok(None);
    };
    if row.expires_at > Timestamp::now() {
        Ok(Some(row))
    } else {
        Ok(None)
    }
}

async fn fetch_card(fetcher: &Fetcher, url: &str) -> Option<Card> {
    let deadline = Instant::now() + fetch_timeout();
    let page = fetcher
        .load(url, "text/html,application/xhtml+xml", deadline)
        .await
        .ok()?;
    if page.json {
        return None;
    }
    let mut card = parse::parse_html(&page.body, &page.url);
    if let Some(oembed) = card.oembed.clone()
        && let Ok(doc) = fetcher.load(&oembed, "application/json", deadline).await
        && json_body(&doc)
    {
        parse::merge_oembed(&mut card, &doc.body, &doc.url);
    }
    card.oembed = None;
    Some(card)
}

fn json_body(doc: &Document) -> bool {
    doc.json || doc.body.trim_start().starts_with('{')
}

fn stored(url: &str, card: Card) -> NewLinkPreview {
    let useful = card.title.is_some() || card.description.is_some() || card.image.is_some();
    NewLinkPreview {
        url: url.to_owned(),
        status: if useful {
            PreviewStatus::Ready
        } else {
            PreviewStatus::Failed
        },
        title: card.title,
        description: card.description,
        image_url: card.image,
        site_name: card.site_name,
        expires_at: expiry(if useful { DAY } else { HOUR }),
    }
}

fn failed(url: &str) -> NewLinkPreview {
    NewLinkPreview {
        url: url.to_owned(),
        status: PreviewStatus::Failed,
        title: None,
        description: None,
        image_url: None,
        site_name: None,
        expires_at: expiry(HOUR),
    }
}

fn expiry(ttl: Duration) -> Timestamp {
    let extra = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
    Timestamp::from_millis(Timestamp::now().as_millis().saturating_add(extra))
}

async fn publish(
    pool: &db::Pool,
    bus: &EventBus,
    message_id: ss_core::Id,
) -> Result<(), db::Error> {
    let mut conn = pool.read().await?;
    let Some(message) = messages::get(&mut conn, message_id).await? else {
        return Ok(());
    };
    let wire = crate::handlers::messages::to_proto(&mut conn, &message).await?;
    drop(conn);
    bus.publish(
        None,
        server_envelope::Msg::MessageUpdated(v1::MessageUpdated {
            message: Some(wire),
            nonce: None,
        }),
    );
    Ok(())
}

fn fetch_timeout() -> Duration {
    Duration::from_secs(5)
}
