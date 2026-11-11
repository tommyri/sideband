//! Turns a queued message into cached preview cards.
//!
//! A fresh URL is fetched once. The same URL is reused until its TTL, 24
//! hours after success and 1 hour after failure. Viewers then get
//! `MessageUpdated`, unless the message was deleted meanwhile: a soft delete
//! keeps the content, so publishing it would show a deleted message again.

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
    let mut ready = Vec::new();
    for (position, url) in urls.iter().enumerate() {
        let preview = resolve(pool, fetcher, url).await?;
        if preview.status == PreviewStatus::Ready {
            ready.push((position, url));
        }
    }
    if ready.is_empty() {
        return Ok(());
    }
    // Check, link and publish under the writer. A delete needs the writer to
    // commit and publishes after it, so it cannot fall between the check and
    // this event, and its `MessageDeleted` always follows it.
    let mut conn = pool.write().await;
    let Some(message) = messages::get(&mut conn, job.message_id).await? else {
        return Ok(());
    };
    if message.deleted_at.is_some() {
        return Ok(());
    }
    for (position, url) in ready {
        link_previews::link(
            &mut conn,
            job.message_id,
            url,
            i64::try_from(position).unwrap_or(i64::MAX),
        )
        .await?;
    }
    let wire = crate::handlers::messages::to_proto(&mut conn, &message).await?;
    bus.publish(
        None,
        server_envelope::Msg::MessageUpdated(v1::MessageUpdated {
            message: Some(wire),
            nonce: None,
        }),
    );
    drop(conn);
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

fn fetch_timeout() -> Duration {
    Duration::from_secs(5)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ss_core::{Id, Timestamp};

    use super::{DAY, enrich, expiry};
    use crate::{
        db::{self, NewLinkPreview, NewMessage, PreviewStatus, link_previews, messages},
        events::EventBus,
        previews::{
            PreviewJob,
            fetch::{Fetcher, Lookup},
        },
    };

    #[tokio::test]
    async fn a_message_deleted_before_its_preview_resolves_is_not_republished() {
        let pool = db::testing::pool().await;
        let url = "https://example.com/page";
        let content = format!("see {url}");
        let message = {
            let mut conn = pool.write().await;
            let author = db::testing::user(&mut conn, 1).await.id;
            let channel = db::testing::text_channel(&mut conn, 2, author).await.id;
            // A cached card, so the worker resolves without a fetch.
            link_previews::upsert(
                &mut conn,
                NewLinkPreview {
                    url: url.to_owned(),
                    status: PreviewStatus::Ready,
                    title: Some("Hello".into()),
                    description: None,
                    image_url: None,
                    site_name: None,
                    expires_at: expiry(DAY),
                },
            )
            .await
            .unwrap();
            let message = messages::insert(
                &mut conn,
                NewMessage {
                    id: Id::generate(),
                    channel_id: channel,
                    author_id: Some(author),
                    webhook_id: None,
                    webhook_name: None,
                    content: content.clone(),
                    reply_to: None,
                    mentions: Vec::new(),
                    mentions_everyone: false,
                },
            )
            .await
            .unwrap();
            // Deleted while its preview job waited in the queue.
            messages::soft_delete(&mut conn, message.id, Timestamp::now())
                .await
                .unwrap();
            message.id
        };
        let bus = EventBus::new();
        let mut events = bus.subscribe();
        let fetcher = Fetcher::new(
            Lookup::Fixed(std::collections::HashMap::new()),
            false,
            Vec::new(),
            Vec::new(),
        );
        enrich(
            &pool,
            &bus,
            &fetcher,
            &PreviewJob {
                message_id: message,
                content,
            },
        )
        .await
        .unwrap();
        let published = tokio::time::timeout(Duration::from_millis(200), events.recv()).await;
        assert!(
            published.is_err(),
            "a deleted message was republished: {published:?}"
        );
        let mut conn = pool.read().await.unwrap();
        assert!(
            link_previews::links(&mut conn, message)
                .await
                .unwrap()
                .is_empty(),
            "a deleted message was linked to a preview"
        );
    }
}
