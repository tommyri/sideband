//! Server settings bridge; credentials remain in the pinned native transport.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use ss_client_core::{
    attachments::AttachmentSession,
    server_settings::{ServerSettings, ServerSettingsSnapshot},
};
use ss_net::pins::PinStore;
use tauri::State;
use ts_rs::TS;
#[derive(Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ServerSettingsFields {
    name: String,
    description: String,
    icon_id: Option<String>,
    registration_mode: String,
    revision: String,
}
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ServerSettingsView {
    fields: ServerSettingsFields,
    version: String,
    fingerprint: String,
    icon: Option<String>,
    icon_warning: Option<String>,
}
impl From<ServerSettingsFields> for ServerSettings {
    fn from(v: ServerSettingsFields) -> Self {
        Self {
            name: v.name,
            description: v.description,
            icon_id: v.icon_id,
            registration_mode: v.registration_mode,
            revision: v.revision,
        }
    }
}
async fn project(
    session: &AttachmentSession,
    version: String,
    fingerprint: String,
    snapshot: ServerSettingsSnapshot,
) -> Result<ServerSettingsView, ApiError> {
    let mut icon = None;
    let mut icon_warning = None;
    if let (Some(id), Some(filename), Some(mime)) = (
        &snapshot.settings.icon_id,
        &snapshot.icon_filename,
        &snapshot.icon_mime,
    ) {
        if matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") {
            let result = async {
                let id = id
                    .parse::<ss_core::Id>()
                    .map_err(|_| ApiError::new("icon", "Invalid icon"))?;
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    session.download(id, filename),
                )
                .await
                .map_err(|_| ApiError::new("icon", "Preview timed out"))?
                .map_err(api_error)
            }
            .await;
            match result {
                Ok(bytes) if bytes.len() <= 2 * 1024 * 1024 => {
                    icon = Some(format!(
                        "data:{mime};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    ))
                }
                _ => icon_warning = Some("The saved icon preview could not be loaded.".into()),
            }
        } else {
            icon_warning = Some("The saved icon uses an unsupported image format.".into());
        }
    }
    let settings = snapshot.settings;
    Ok(ServerSettingsView {
        fields: ServerSettingsFields {
            name: settings.name,
            description: settings.description,
            icon_id: settings.icon_id,
            registration_mode: settings.registration_mode,
            revision: settings.revision,
        },
        version,
        fingerprint,
        icon,
        icon_warning,
    })
}
async fn load(
    state: &AppState,
    server_id: &str,
    fields: Option<ServerSettingsFields>,
) -> Result<ServerSettingsView, ApiError> {
    let server = parse_server_id(server_id)?;
    let fingerprint = state
        .pins
        .get(&server)
        .map_err(|e| ApiError::new("pin", e.to_string()))?
        .map_or_else(String::new, |p| p.to_string());
    let (session, version) = {
        let clients = state.clients.lock().await;
        let client = clients
            .get(&server)
            .ok_or_else(|| ApiError::new("disconnected", "Server is disconnected"))?;
        let version = client
            .state()
            .read()
            .server
            .as_ref()
            .map_or_else(String::new, |s| s.version.clone());
        (
            client.attachment_session().await.map_err(api_error)?,
            version,
        )
    };
    let snapshot = match fields {
        Some(fields) => session.save_server_settings(&fields.into()).await,
        None => session.server_settings().await,
    }
    .map_err(api_error)?;
    project(&session, version, fingerprint, snapshot).await
}
#[tauri::command]
pub async fn get_server_settings(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerSettingsView, ApiError> {
    load(&state, &server_id, None).await
}
#[tauri::command]
pub async fn save_server_settings(
    state: State<'_, AppState>,
    server_id: String,
    fields: ServerSettingsFields,
) -> Result<ServerSettingsView, ApiError> {
    load(&state, &server_id, Some(fields)).await
}
#[tauri::command]
pub async fn upload_server_icon(
    state: State<'_, AppState>,
    server_id: String,
    data: String,
) -> Result<String, ApiError> {
    if data.len() > (2 * 1024 * 1024_usize).div_ceil(3) * 4 {
        return Err(ApiError::new("icon", "Choose an image up to 2 MiB"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| ApiError::new("icon", "Invalid image data"))?;
    let server = parse_server_id(&server_id)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| ApiError::new("disconnected", "Server is disconnected"))?;
    let session = client.attachment_session().await.map_err(api_error)?;
    drop(clients);
    session.upload_server_icon(bytes).await.map_err(api_error)
}
