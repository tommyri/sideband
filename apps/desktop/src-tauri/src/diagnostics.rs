//! Safe diagnostics projection. Never returns auth, server addresses or message contents.
use crate::{state::AppState, types::ApiError};
use tauri::State;
#[tauri::command]
pub async fn diagnostics_snapshot(
    state: State<'_, AppState>,
    server_id: Option<String>,
) -> Result<serde_json::Value, ApiError> {
    let clients = state.clients.lock().await;
    let voice = state.voice.lock().await;
    let selected = voice
        .as_ref()
        .map(|v| v.server.as_str())
        .or(server_id.as_deref());
    let client = selected.and_then(|id| {
        clients
            .iter()
            .find(|(key, _)| key.as_str() == id)
            .map(|(_, c)| c)
    });
    let transport = client.and_then(ss_client_core::Client::diagnostics);
    let stats = voice
        .as_ref()
        .map(|v| crate::commands::voice_stats(v.session.stats()));
    // The existing bridge includes user IDs. Remove them from this diagnostics boundary.
    let mut stats = serde_json::to_value(stats)
        .map_err(|_| ApiError::new("diagnostics", "Could not serialize diagnostics"))?;
    if let Some(speakers) = stats
        .get_mut("speakers")
        .and_then(serde_json::Value::as_array_mut)
    {
        for speaker in speakers {
            if let Some(object) = speaker.as_object_mut() {
                object.remove("user");
            }
        }
    }
    Ok(serde_json::json!({"transport":transport,"voice":stats,
        "details":voice.as_ref().and_then(|v|v.session.diagnostics()),
        "unavailable":{"migrations":"Migration count is not exposed by Quinn",
            "bufferSizes":"Negotiated hardware buffer sizes are not exposed",
            "acousticLatency":"Not measured; use the calibrated guided test"}}))
}
