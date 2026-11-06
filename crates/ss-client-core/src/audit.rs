//! Bounded audit pages over the authenticated, pinned HTTPS connection.
use base64::Engine;
use serde::Deserialize;
use ss_core::Id;

use crate::{Client, Error, http::read_bounded};

/// Filters combined with AND, before the page limit is applied.
#[derive(Default)]
pub struct AuditFilter {
    /// Exact actor id; absent includes system actions.
    pub actor: Option<Id>,
    /// Exact action name.
    pub action: Option<String>,
    /// Inclusive Unix milliseconds.
    pub since: Option<i64>,
    /// Exclusive Unix milliseconds.
    pub until: Option<i64>,
}

/// One server audit record. Details are data, never executable markup.
pub struct AuditEntry {
    /// Stable page cursor.
    pub id: Id,
    /// Empty for system actions.
    pub actor_id: Option<Id>,
    /// Server action name.
    pub action: String,
    /// Kind of entity affected.
    pub target_kind: String,
    /// Target, when applicable.
    pub target_id: Option<Id>,
    /// Structured action-specific metadata.
    pub details: serde_json::Value,
    /// Unix milliseconds.
    pub created_at: i64,
}

impl Client {
    /// Fetch up to 50 audit rows; requires VIEW_AUDIT_LOG on the server.
    pub async fn audit_page(
        &self,
        before: Option<Id>,
        filter: &AuditFilter,
    ) -> Result<Vec<AuditEntry>, Error> {
        let session = self.http_session()?;
        let mut query = vec![("limit", "50".to_owned())];
        for (key, value) in [
            ("before", before.map(|id| id.to_string())),
            ("actor", filter.actor.map(|id| id.to_string())),
            ("action", filter.action.clone()),
            ("since", filter.since.map(|v| v.to_string())),
            ("until", filter.until.map(|v| v.to_string())),
        ] {
            if let Some(value) = value {
                query.push((key, value));
            }
        }
        let response = session
            .http
            .get(session.base.join("api/v1/audit-log").expect("static URL"))
            .bearer_auth(&session.token)
            .timeout(std::time::Duration::from_secs(10))
            .query(&query)
            .send()
            .await?;
        let body = read_bounded(response, 1024 * 1024).await?;
        let rows: Vec<Row> = serde_json::from_slice(&body)?;
        if rows.len() > 50 {
            return Err(Error::Protocol("oversized audit page"));
        }
        rows.into_iter().map(Row::into_entry).collect()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    id: String,
    actor_id: Option<String>,
    action: String,
    target_kind: String,
    target_id: Option<String>,
    details: serde_json::Value,
    created_at: String,
}
fn id(value: &str) -> Result<Id, Error> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| Error::Protocol("invalid audit identifier"))?;
    Id::try_from(bytes.as_slice()).map_err(|_| Error::Protocol("invalid audit identifier"))
}
impl Row {
    fn into_entry(self) -> Result<AuditEntry, Error> {
        let created_at: i64 = self
            .created_at
            .parse()
            .map_err(|_| Error::Protocol("invalid audit timestamp"))?;
        if !(-8_640_000_000_000_000..=8_640_000_000_000_000).contains(&created_at) {
            return Err(Error::Protocol(
                "audit timestamp outside supported date range",
            ));
        }
        Ok(AuditEntry {
            id: id(&self.id)?,
            actor_id: self.actor_id.as_deref().map(id).transpose()?,
            action: self.action,
            target_kind: self.target_kind,
            target_id: self.target_id.as_deref().map(id).transpose()?,
            details: self.details,
            created_at,
        })
    }
}
