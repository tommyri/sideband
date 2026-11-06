//! Invite metadata pages over pinned HTTPS; codes are available only from CreateInvite.
use crate::{Client, Error, http::read_bounded, v1};
use base64::Engine;
use serde::Deserialize;
use ss_core::Id;
impl Client {
    /// Get at most 50 invite records visible to the authenticated account.
    pub async fn list_invites(&self, before: Option<Id>) -> Result<Vec<v1::Invite>, Error> {
        let session = self.http_session()?;
        let mut request = session
            .http
            .get(session.base.join("api/v1/invites").expect("static URL"))
            .bearer_auth(&session.token)
            .timeout(std::time::Duration::from_secs(10))
            .query(&[("limit", "50")]);
        if let Some(before) = before {
            request = request.query(&[("before", before.to_string())]);
        }
        let response = request.send().await?;
        let body = read_bounded(response, 256 * 1024).await?;
        let rows: Vec<Row> = serde_json::from_slice(&body)?;
        if rows.len() > 50 {
            return Err(Error::Protocol("oversized invite page"));
        }
        rows.into_iter().map(Row::into_wire).collect()
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    id: String,
    created_by: String,
    max_uses: Option<u32>,
    uses: u32,
    created_at: String,
    expires_at: Option<String>,
    revoked_at: Option<String>,
}
fn id(value: &str) -> Result<bytes::Bytes, Error> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| Error::Protocol("invalid invite identifier"))?;
    let _ =
        Id::try_from(bytes.as_slice()).map_err(|_| Error::Protocol("invalid invite identifier"))?;
    Ok(bytes.into())
}
fn stamp(value: &str) -> Result<i64, Error> {
    value
        .parse()
        .map_err(|_| Error::Protocol("invalid invite timestamp"))
}
impl Row {
    fn into_wire(self) -> Result<v1::Invite, Error> {
        Ok(v1::Invite {
            id: id(&self.id)?,
            created_by: id(&self.created_by)?,
            code: String::new(),
            max_uses: self.max_uses,
            uses: self.uses,
            created_at: stamp(&self.created_at)?,
            expires_at: self.expires_at.as_deref().map(stamp).transpose()?,
            revoked_at: self.revoked_at.as_deref().map(stamp).transpose()?,
        })
    }
}
