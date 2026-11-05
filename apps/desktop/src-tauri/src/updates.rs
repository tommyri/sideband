//! Only the release-pinned feed/key can supply updates. Installation is explicit.
use crate::{state::AppState, types::ApiError};
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct Pending(Mutex<Option<Update>>);
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    enabled: bool,
    version: Option<String>,
    notes: Option<String>,
}
fn enabled() -> bool {
    !env!("SS_UPDATE_PUBLIC_KEY").is_empty()
        && env!("SS_UPDATE_ENDPOINT").starts_with("https://")
        && (!cfg!(target_os = "linux") || std::env::var_os("APPIMAGE").is_some())
}
fn error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new("update", e.to_string())
}
#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    pending: State<'_, Pending>,
) -> Result<UpdateStatus, ApiError> {
    let mut slot = pending.0.lock().await;
    if !enabled() {
        return Ok(UpdateStatus {
            enabled: false,
            version: None,
            notes: None,
        });
    }
    let update = app
        .updater_builder()
        .pubkey(env!("SS_UPDATE_PUBLIC_KEY"))
        .endpoints(vec![env!("SS_UPDATE_ENDPOINT").parse().map_err(error)?])
        .map_err(error)?
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(error)?
        .check()
        .await
        .map_err(error)?;
    let result = UpdateStatus {
        enabled: true,
        version: update.as_ref().map(|u| u.version.clone()),
        notes: update.as_ref().and_then(|u| u.body.clone()),
    };
    *slot = update;
    Ok(result)
}
#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    pending: State<'_, Pending>,
    state: State<'_, AppState>,
) -> Result<(), ApiError> {
    let slot = pending.0.lock().await;
    let update = slot
        .as_ref()
        .ok_or_else(|| error("Check for an update first."))?;
    // Prevent new calls/device tests while installing; don't interrupt an existing call.
    let _operation = state.operations.lock().await;
    if state.voice.lock().await.is_some() || state.audio_test.lock().await.is_some() {
        return Err(error(
            "Leave your voice call and stop audio tests before updating.",
        ));
    }
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(error)?;
    app.restart();
}
