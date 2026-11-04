//! Explicit, cancellable transfers. No bearer tokens or server URLs reach the webview.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
};
use base64::Engine;
use serde::Deserialize;
use ss_client_core::attachments::MAX_FILE_BYTES;
use ss_core::Id;
use std::{io::Write, path::Path};
use tauri::{Manager, State};
use tokio::sync::watch;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRequest {
    transfer: String,
    server: String,
    channel: Option<String>,
    attachment: Option<String>,
    filename: String,
    data: Option<String>,
    save: bool,
    preview: Option<String>,
}
fn error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new("attachment", e.to_string())
}
fn id(value: Option<&str>) -> Result<Id, ApiError> {
    value.unwrap_or_default().parse().map_err(error)
}
fn safe_filename(value: &str) -> String {
    let name: String = value
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:<>\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(180)
        .collect();
    let name = name.trim_matches([' ', '.']);
    if name.is_empty() {
        "file".into()
    } else {
        name.to_owned()
    }
}
fn save_file(folder: &Path, filename: &str, data: &[u8]) -> Result<String, ApiError> {
    std::fs::create_dir_all(folder).map_err(error)?;
    // Prefix avoids reserved Windows names and collisions; never overwrite a local file.
    let path = folder.join(format!("{}-{}", Id::generate(), safe_filename(filename)));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(error)?;
    if let Err(e) = file.write_all(data) {
        let _ = std::fs::remove_file(&path);
        return Err(error(e));
    }
    Ok(path.to_string_lossy().into_owned())
}
#[tauri::command]
pub async fn attachment_transfer(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: FileRequest,
) -> Result<String, ApiError> {
    if request.transfer.is_empty() || request.transfer.len() > 100 {
        return Err(error("Invalid transfer identifier"));
    }
    let (cancel, mut cancelled) = watch::channel(false);
    {
        let mut transfers = state.transfers.lock().await;
        if transfers.len() >= 10 || transfers.contains_key(&request.transfer) {
            return Err(error("Too many active transfers"));
        }
        transfers.insert(request.transfer.clone(), cancel);
    }
    let result = tokio::select! {
        _ = cancelled.changed() => Err(error("Transfer cancelled")),
        result = transfer(&app, &state, &request) => result,
    };
    state.transfers.lock().await.remove(&request.transfer);
    result
}
async fn transfer(
    app: &tauri::AppHandle,
    state: &AppState,
    request: &FileRequest,
) -> Result<String, ApiError> {
    if let Some(url) = &request.preview {
        return preview_image(url).await;
    }
    let session = {
        let server = parse_server_id(&request.server)?;
        let clients = state.clients.lock().await;
        clients
            .get(&server)
            .ok_or_else(|| error("Server is disconnected"))?
            .attachment_session()
            .await
            .map_err(api_error)?
    };
    if let Some(data) = &request.data {
        if data.len() > MAX_FILE_BYTES.div_ceil(3) * 4 {
            return Err(error("Files must be at most 25 MiB"));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(error)?;
        return session
            .upload(
                id(request.channel.as_deref())?,
                request.filename.clone(),
                bytes,
            )
            .await
            .map(|id| id.to_string())
            .map_err(api_error);
    }
    let bytes = session
        .download(id(request.attachment.as_deref())?, &request.filename)
        .await
        .map_err(api_error)?;
    if request.save {
        let folder = app
            .path()
            .download_dir()
            .map_err(error)?
            .join("Sideband downloads");
        // Saving a completed transfer is atomic with respect to cancellation.
        save_file(&folder, &request.filename, &bytes)
    } else {
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }
}
#[tauri::command]
pub async fn cancel_attachment(
    state: State<'_, AppState>,
    transfer: String,
) -> Result<(), ApiError> {
    if let Some(cancel) = state.transfers.lock().await.get(&transfer) {
        let _ = cancel.send(true);
    }
    Ok(())
}
// External preview images are opt-in, credential-free and restricted to public
// HTTPS addresses. Resolve once, validate every answer, and pin DNS for the socket.
async fn preview_image(value: &str) -> Result<String, ApiError> {
    let url = reqwest::Url::parse(value).map_err(error)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return Err(error(
            "Preview images require a public HTTPS URL on port 443",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| error("Missing image host"))?
        .trim_matches(['[', ']']);
    let addresses: Vec<_> = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::lookup_host((host, 443)),
    )
    .await
    .map_err(error)?
    .map_err(error)?
    .collect();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|a| !ss_net::public_address::is_public(a.ip()))
    {
        return Err(error("Preview images cannot access private networks"));
    }
    let http = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addresses)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(error)?;
    let mut response = http
        .get(url.clone())
        .header("Accept", "image/png,image/jpeg,image/gif,image/webp")
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?;
    if !response.status().is_success() {
        return Err(error("Image redirects are not followed"));
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !matches!(
        mime.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(error("Unsupported preview image type"));
    }
    const MAX: usize = 4 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|len| len > MAX as u64)
    {
        return Err(error("Preview image is too large"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(error)? {
        if bytes.len().saturating_add(chunk.len()) > MAX {
            return Err(error("Preview image is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::json!({ "mime": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes) }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn downloads_cannot_escape_the_folder_or_overwrite_files() {
        let dir = tempfile::tempdir().unwrap();
        let first = save_file(dir.path(), "../../CON:<bad>\\name", b"first").unwrap();
        let second = save_file(dir.path(), "../../CON:<bad>\\name", b"second").unwrap();
        assert_ne!(first, second);
        assert_eq!(Path::new(&first).parent(), Some(dir.path()));
        assert_eq!(std::fs::read(first).unwrap(), b"first");
        assert_eq!(safe_filename("..."), "file");
    }
    #[tokio::test]
    async fn preview_images_reject_credentials_schemes_and_private_hosts() {
        for url in [
            "http://example.com/image",
            "file:///etc/passwd",
            "https://user:pass@example.com/image",
            "https://example.com:444/image",
            "https://127.0.0.1/image",
            "https://[::1]/image",
        ] {
            assert!(preview_image(url).await.is_err(), "{url}");
        }
    }
}
