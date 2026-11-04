//! Webhook metadata and one-time creation URLs over the approved server identity.
use crate::{Client, Error, http::read_bounded, v1};
use base64::Engine;
use serde::Deserialize;
use ss_core::Id;

/// Metadata never contains the posting credential.
pub struct WebhookMetadata {
    /// Webhook identifier and cursor.
    pub id: Id,
    /// Destination text channel.
    pub channel_id: Id,
    /// Default bot display name.
    pub name: String,
    /// Creator.
    pub created_by: Id,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Revocation time in Unix milliseconds.
    pub revoked_at: Option<i64>,
}
/// One-time secret returned only from creation. Deliberately not Debug or persisted.
pub struct CreatedWebhookUrl {
    /// Created webhook.
    pub id: Id,
    /// HTTPS posting URL, including its secret token.
    pub url: String,
}
impl Client {
    /// Fetch up to 50 records. Requires MANAGE_SERVER.
    pub async fn list_webhooks(&self, before: Option<Id>) -> Result<Vec<WebhookMetadata>, Error> {
        let session = self.http_session().await?;
        let mut request = session
            .http
            .get(session.base.join("api/v1/webhooks").expect("static URL"))
            .bearer_auth(&session.token)
            .timeout(std::time::Duration::from_secs(10))
            .query(&[("limit", "50")]);
        if let Some(before) = before {
            request = request.query(&[("before", before.to_string())]);
        }
        let body = read_bounded(request.send().await?, 256 * 1024).await?;
        let rows: Vec<Row> = serde_json::from_slice(&body)?;
        if rows.len() > 50 {
            return Err(Error::Protocol("oversized webhook page"));
        }
        rows.into_iter().map(Row::into_metadata).collect()
    }
    /// Create once, returning a URL based on the connected HTTPS origin, never a server-supplied URL.
    pub async fn create_webhook_url(
        &self,
        command: v1::CreateWebhook,
    ) -> Result<CreatedWebhookUrl, Error> {
        // Resolve the origin and pin before the mutation, avoiding a post-create setup failure.
        let session = self.http_session().await?;
        let webhook = self.create_webhook(command).await?;
        let id = Id::try_from(webhook.id.as_ref())
            .map_err(|_| Error::Protocol("invalid webhook identifier"))?;
        let token = webhook
            .token
            .filter(|t| !t.is_empty())
            .ok_or(Error::Protocol("missing webhook token"))?;
        let mut url = session.base;
        url.path_segments_mut()
            .map_err(|()| Error::Protocol("invalid webhook origin"))?
            .extend(["api", "v1", "webhooks", &id.to_string(), &token]);
        Ok(CreatedWebhookUrl {
            id,
            url: url.into(),
        })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    id: String,
    channel_id: String,
    name: String,
    created_by: String,
    created_at: String,
    revoked_at: Option<String>,
}
fn id(value: &str) -> Result<Id, Error> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| Error::Protocol("invalid webhook identifier"))?;
    Id::try_from(bytes.as_slice()).map_err(|_| Error::Protocol("invalid webhook identifier"))
}
fn stamp(value: &str) -> Result<i64, Error> {
    let value = value
        .parse::<i64>()
        .map_err(|_| Error::Protocol("invalid webhook timestamp"))?;
    if !(-8_640_000_000_000_000..=8_640_000_000_000_000).contains(&value) {
        return Err(Error::Protocol(
            "webhook timestamp outside supported date range",
        ));
    }
    Ok(value)
}
impl Row {
    fn into_metadata(self) -> Result<WebhookMetadata, Error> {
        Ok(WebhookMetadata {
            id: id(&self.id)?,
            channel_id: id(&self.channel_id)?,
            name: self.name,
            created_by: id(&self.created_by)?,
            created_at: stamp(&self.created_at)?,
            revoked_at: self.revoked_at.as_deref().map(stamp).transpose()?,
        })
    }
}
