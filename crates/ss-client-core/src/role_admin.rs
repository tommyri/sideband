//! Read raw channel overrides using the authenticated, pinned HTTPS connection.
use crate::{Client, Error, http::read_bounded};
use serde::{Deserialize, Serialize};
/// A stored per-role override. Masks are decimal strings to preserve all 64 bits.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelOverride {
    /// Role ULID.
    pub role_id: String,
    /// Explicit allows.
    pub allow: String,
    /// Explicit denials.
    pub deny: String,
}
impl Client {
    /// Requires MANAGE_CHANNELS and visibility of this guild channel.
    pub async fn channel_overrides(
        &self,
        channel: ss_core::Id,
    ) -> Result<Vec<ChannelOverride>, Error> {
        let session = self.http_session().await?;
        let url = session
            .base
            .join(&format!("api/v1/channels/{channel}/overrides"))
            .map_err(|_| Error::Protocol("invalid origin"))?;
        let response = session
            .http
            .get(url)
            .bearer_auth(&session.token)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;
        let body = read_bounded(response, 1024 * 1024).await?;
        let rows: Vec<ChannelOverride> = serde_json::from_slice(&body)?;
        for row in &rows {
            let _ = row
                .role_id
                .parse::<ss_core::Id>()
                .map_err(|_| Error::Protocol("invalid role"))?;
            row.allow
                .parse::<u64>()
                .map_err(|_| Error::Protocol("invalid allow"))?;
            row.deny
                .parse::<u64>()
                .map_err(|_| Error::Protocol("invalid deny"))?;
        }
        Ok(rows)
    }
}
