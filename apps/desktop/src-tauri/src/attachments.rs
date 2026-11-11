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
    // Marked before the first byte, so no complete unmarked copy ever exists.
    // A file that cannot be marked is removed rather than left unchecked.
    let written = mark_downloaded(&path, &file)
        .map_err(|e| error(format!("Could not mark the file as downloaded: {e}")))
        .and_then(|()| file.write_all(data).map_err(error));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path.to_string_lossy().into_owned())
}
/// Sets `com.apple.quarantine` in the format Apple's downloaders use, so
/// Gatekeeper checks the file when it is opened. Per file rather than
/// `LSFileQuarantineEnabled`, which would also quarantine the app's own
/// storage and the updater's files, and does not apply to unbundled builds.
#[cfg(target_os = "macos")]
fn mark_downloaded(_path: &Path, file: &std::fs::File) -> std::io::Result<()> {
    use std::fmt::Write as _;
    use xattr::FileExt;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_secs();
    // Flags 0x83, as on browser downloads. The event id is a
    // fresh ULID in UUID form; no Launch Services event record is written.
    let mut value = format!("0083;{now:08x};Sideband;");
    for (i, byte) in Id::generate().as_bytes().iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            value.push('-');
        }
        let _ = write!(value, "{byte:02X}");
    }
    file.set_xattr("com.apple.quarantine", value.as_bytes())
}
/// Writes the Mark of the Web (Internet zone), which SmartScreen and the
/// "downloaded from the internet" prompt read. First written on a Mac
/// without a Windows target, so it was not compiled there (T-104).
#[cfg(windows)]
fn mark_downloaded(path: &Path, _file: &std::fs::File) -> std::io::Result<()> {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::fs::write(stream, b"[ZoneTransfer]\r\nZoneId=3\r\n")
}
/// Linux has no download mark that the desktop checks before running a file.
#[cfg(not(any(target_os = "macos", windows)))]
#[allow(clippy::unnecessary_wraps)] // Same signature as the macOS and Windows marks.
fn mark_downloaded(_path: &Path, _file: &std::fs::File) -> std::io::Result<()> {
    Ok(())
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
        result = transfer(&state, &request) => result,
    };
    state.transfers.lock().await.remove(&request.transfer);
    match result? {
        Transferred::Reply(reply) => Ok(reply),
        // Saving a completed download is not cancelled: once the bytes are
        // here the file is written whole, on a blocking worker.
        Transferred::Save(bytes) => {
            let folder = app
                .path()
                .download_dir()
                .map_err(error)?
                .join("Sideband downloads");
            let filename = request.filename;
            tauri::async_runtime::spawn_blocking(move || save_file(&folder, &filename, &bytes))
                .await
                .map_err(error)?
        }
    }
}
/// A finished transfer: the reply for the webview, or a download to save.
enum Transferred {
    Reply(String),
    Save(Vec<u8>),
}
async fn transfer(state: &AppState, request: &FileRequest) -> Result<Transferred, ApiError> {
    if let Some(url) = &request.preview {
        return preview_image(url).await.map(Transferred::Reply);
    }
    let session = {
        let server = parse_server_id(&request.server)?;
        state
            .client(&server)
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
            .map(|id| Transferred::Reply(id.to_string()))
            .map_err(api_error);
    }
    let bytes = session
        .download(id(request.attachment.as_deref())?, &request.filename)
        .await
        .map_err(api_error)?;
    Ok(if request.save {
        Transferred::Save(bytes)
    } else {
        Transferred::Reply(base64::engine::general_purpose::STANDARD.encode(bytes))
    })
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
    const MAX: usize = 4 * 1024 * 1024;
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
    #[cfg(target_os = "macos")]
    #[test]
    fn saved_downloads_carry_the_quarantine_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_file(dir.path(), "run me.command", b"#!/bin/sh\n").unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let value = xattr::get(&path, "com.apple.quarantine")
            .unwrap()
            .expect("a saved download has com.apple.quarantine");
        let value = String::from_utf8(value).unwrap();
        let fields: Vec<&str> = value.split(';').collect();
        assert_eq!(fields.len(), 4, "{value}");
        assert_eq!(fields[0], "0083", "{value}");
        let at = u64::from_str_radix(fields[1], 16).unwrap();
        assert!(at <= now && now - at < 60, "{value}");
        assert_eq!(fields[1], format!("{at:08x}"), "{value}");
        assert_eq!(fields[2], "Sideband", "{value}");
        let groups: Vec<usize> = fields[3].split('-').map(str::len).collect();
        assert_eq!(groups, [8, 4, 4, 4, 12], "{value}");
        assert!(
            fields[3]
                .chars()
                .all(|c| c == '-' || c.is_ascii_digit() || c.is_ascii_uppercase()),
            "{value}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"#!/bin/sh\n");
    }
    #[cfg(windows)]
    #[test]
    fn saved_downloads_carry_the_mark_of_the_web() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_file(dir.path(), "setup.exe", b"MZ").unwrap();
        let stream = std::fs::read(format!("{path}:Zone.Identifier")).unwrap();
        assert_eq!(stream, b"[ZoneTransfer]\r\nZoneId=3\r\n");
        assert_eq!(std::fs::read(&path).unwrap(), b"MZ");
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
