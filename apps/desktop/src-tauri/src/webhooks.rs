//! Webhook management bridge. Only creation reveals a credential-bearing URL.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
};
use serde::Serialize;
use ss_client_core::{WebhookMetadata, v1};
use ss_core::Id;
use tauri::State;
use ts_rs::TS;
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct WebhookView {
    id: String,
    channel_id: String,
    name: String,
    created_by: String,
    #[ts(type = "number")]
    created_at: i64,
    #[ts(type = "number | null")]
    revoked_at: Option<i64>,
}
#[derive(Serialize, TS)]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct CreatedWebhook {
    id: String,
    url: String,
}
impl From<WebhookMetadata> for WebhookView {
    fn from(row: WebhookMetadata) -> Self {
        Self {
            id: row.id.to_string(),
            channel_id: row.channel_id.to_string(),
            name: row.name,
            created_by: row.created_by.to_string(),
            created_at: row.created_at,
            revoked_at: row.revoked_at,
        }
    }
}
fn parse_id(value: &str) -> Result<Id, ApiError> {
    value
        .parse()
        .map_err(|_| ApiError::new("invalid_webhook", "Identifier must be a ULID"))
}
fn disconnected() -> ApiError {
    ApiError::new("disconnected", "Server is disconnected")
}
#[tauri::command]
pub async fn webhook_page(
    state: State<'_, AppState>,
    server_id: String,
    before: Option<String>,
) -> Result<Vec<WebhookView>, ApiError> {
    let server = parse_server_id(&server_id)?;
    let before = before.as_deref().map(parse_id).transpose()?;
    let clients = state.clients.lock().await;
    let client = clients.get(&server).ok_or_else(disconnected)?;
    Ok(client
        .list_webhooks(before)
        .await
        .map_err(api_error)?
        .into_iter()
        .map(WebhookView::from)
        .collect())
}
#[tauri::command]
pub async fn create_server_webhook(
    state: State<'_, AppState>,
    server_id: String,
    channel_id: String,
    name: String,
) -> Result<CreatedWebhook, ApiError> {
    let server = parse_server_id(&server_id)?;
    let channel = parse_id(&channel_id)?;
    let clients = state.clients.lock().await;
    let client = clients.get(&server).ok_or_else(disconnected)?;
    let created = client
        .create_webhook_url(v1::CreateWebhook {
            channel_id: channel.as_bytes().to_vec().into(),
            name,
        })
        .await
        .map_err(api_error)?;
    Ok(CreatedWebhook {
        id: created.id.to_string(),
        url: created.url,
    })
}
#[tauri::command]
pub async fn revoke_server_webhook(
    state: State<'_, AppState>,
    server_id: String,
    webhook_id: String,
) -> Result<(), ApiError> {
    let server = parse_server_id(&server_id)?;
    let webhook = parse_id(&webhook_id)?;
    let clients = state.clients.lock().await;
    let client = clients.get(&server).ok_or_else(disconnected)?;
    client
        .delete_webhook(v1::DeleteWebhook {
            webhook_id: webhook.as_bytes().to_vec().into(),
        })
        .await
        .map_err(api_error)
}
