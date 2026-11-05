//! Search command for the current authenticated server.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;
#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct SearchQuery {
    q: String,
    channel: Option<String>,
    from: Option<String>,
    before: Option<f64>,
    after: Option<f64>,
    has: Option<String>,
}
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct SearchResult {
    id: String,
    channel_id: String,
    author_id: Option<String>,
    author_name: String,
    created_at: String,
    excerpt: String,
}
fn stamp(value: Option<f64>) -> Result<Option<i64>, ApiError> {
    value
        .map(|v| {
            if !v.is_finite() || v.fract() != 0.0 || v.abs() > 8_640_000_000_000_000.0 {
                return Err(ApiError::new("search", "Invalid date"));
            }
            // Validated finite integral milliseconds in JavaScript's date range.
            #[allow(clippy::cast_possible_truncation)]
            Ok(v as i64)
        })
        .transpose()
}
#[tauri::command]
pub async fn search_messages(
    state: State<'_, AppState>,
    server_id: String,
    query: SearchQuery,
) -> Result<Vec<SearchResult>, ApiError> {
    let request = ss_client_core::search::SearchRequest {
        q: query.q,
        channel: query.channel,
        from: query.from,
        before: stamp(query.before)?,
        after: stamp(query.after)?,
        has: query.has,
    };
    let server = parse_server_id(&server_id)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| ApiError::new("disconnected", "Server is disconnected"))?;
    let session = client.search_session().await.map_err(api_error)?;
    drop(clients);
    Ok(session
        .search_messages(&request)
        .await
        .map_err(api_error)?
        .into_iter()
        .map(|r| SearchResult {
            id: r.id,
            channel_id: r.channel_id,
            author_id: r.author_id,
            author_name: r.author_name,
            created_at: r.created_at,
            excerpt: r.excerpt,
        })
        .collect())
}
