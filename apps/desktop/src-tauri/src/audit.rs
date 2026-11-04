//! Native audit-log bridge. Bearer credentials stay in the core client.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
};
use serde::{Deserialize, Serialize};
use ss_client_core::{AuditEntry, AuditFilter};
use ss_core::Id;
use tauri::State;
use ts_rs::TS;

#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct AuditQuery {
    server_id: String,
    before: Option<String>,
    actor: Option<String>,
    action: Option<String>,
    #[ts(type = "number | null")]
    since: Option<i64>,
    #[ts(type = "number | null")]
    until: Option<i64>,
}
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct AuditEntryView {
    id: String,
    actor_id: Option<String>,
    action: String,
    target_kind: String,
    target_id: Option<String>,
    details: String,
    #[ts(type = "number")]
    created_at: i64,
}
impl From<AuditEntry> for AuditEntryView {
    fn from(entry: AuditEntry) -> Self {
        Self {
            id: entry.id.to_string(),
            actor_id: entry.actor_id.map(|id| id.to_string()),
            action: entry.action,
            target_kind: entry.target_kind,
            target_id: entry.target_id.map(|id| id.to_string()),
            details: serde_json::to_string_pretty(&entry.details).unwrap_or_default(),
            created_at: entry.created_at,
        }
    }
}
fn parse_id(value: Option<String>) -> Result<Option<Id>, ApiError> {
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| ApiError::new("invalid_audit_query", "Identifiers must be ULIDs"))
        })
        .transpose()
}
#[tauri::command]
pub async fn audit_page(
    state: State<'_, AppState>,
    query: AuditQuery,
) -> Result<Vec<AuditEntryView>, ApiError> {
    let server = parse_server_id(&query.server_id)?;
    let before = parse_id(query.before)?;
    let filter = AuditFilter {
        actor: parse_id(query.actor)?,
        action: query.action,
        since: query.since,
        until: query.until,
    };
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| ApiError::new("disconnected", "Server is disconnected"))?;
    Ok(client
        .audit_page(before, &filter)
        .await
        .map_err(api_error)?
        .into_iter()
        .map(AuditEntryView::from)
        .collect())
}
