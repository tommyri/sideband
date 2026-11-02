//! `POST /api/v1/attachments` and `GET /api/v1/attachments/{id}/{filename}`.
//!
//! Upload is bearer auth plus `ATTACH_FILES` on the form's channel id.
//! Download accepts that bearer token or a short-lived `token` query signed
//! for the attachment. Images and video are inline; everything else is an
//! attachment download.

use std::{fmt::Write, sync::Arc};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use ss_core::{ChannelKind, Id, Permissions};

use crate::{
    auth::Auth,
    db::{self, attachments, channels, messages, settings, users},
    limits::{KeyedLimiter, UPLOAD_BURST, UPLOAD_PERIOD, quota},
    perms::{AccessError, PermissionService},
    storage::{SaveError, Saved, Store},
};

/// Auth, permissions, and the file store for the upload routes.
#[derive(Clone)]
pub struct FilesState {
    /// Session-token lookup.
    pub auth: Arc<Auth>,
    /// Channel upload permission.
    pub perms: Arc<PermissionService>,
    /// Blobs, quotas, and signed URLs.
    pub files: Store,
    /// Uploads per account, ahead of the daily byte quota.
    uploads: Arc<KeyedLimiter<Id>>,
}

/// Upload and download routes. Mount them on the public HTTPS listener.
pub fn router(auth: Arc<Auth>, perms: Arc<PermissionService>, files: Store) -> Router {
    let limit = files.body_limit();
    Router::new()
        .route("/api/v1/attachments", post(upload))
        .route(
            "/api/v1/attachments/{attachment_id}/{filename}",
            get(download),
        )
        .layer(DefaultBodyLimit::max(limit))
        .with_state(FilesState {
            auth,
            perms,
            files,
            uploads: Arc::new(KeyedLimiter::new(quota(UPLOAD_BURST, UPLOAD_PERIOD))),
        })
}

#[derive(Deserialize)]
struct DownloadQuery {
    token: Option<String>,
}

async fn upload(
    State(state): State<FilesState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<Value>, HttpError> {
    let user = caller(&state.auth, &headers).await?;
    state.uploads.check(&user).map_err(HttpError::RateLimited)?;
    let (channel_text, filename, bytes) =
        read_upload(&mut multipart, state.files.max_bytes()).await?;
    let channel_id = parse_id(channel_text.trim())?;
    permit_upload(&state, user, channel_id).await?;
    let saved = state
        .files
        .save(user, &filename, &bytes)
        .await
        .map_err(save_error)?;
    let url = state.files.download_url(saved.id, &saved.filename);
    Ok(Json(saved_json(&saved, &url)))
}

async fn download(
    State(state): State<FilesState>,
    Path((attachment_id, filename)): Path<(String, String)>,
    Query(query): Query<DownloadQuery>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let id = parse_id(&attachment_id)?;
    if query
        .token
        .as_deref()
        .is_some_and(|token| state.files.authorize(id, token))
    {
        let file = load_file(&state, id).await?;
        return named(&state, &file, &filename, &headers).await;
    }
    let user = caller(&state.auth, &headers).await?;
    let file = load_file(&state, id).await?;
    if file.filename != filename {
        return Err(HttpError::NotFound);
    }
    may_read(&state, user, &file).await?;
    serve(&state, &file, &headers).await
}

async fn named(
    state: &FilesState,
    file: &db::Attachment,
    filename: &str,
    headers: &HeaderMap,
) -> Result<Response, HttpError> {
    if file.filename != filename {
        return Err(HttpError::NotFound);
    }
    serve(state, file, headers).await
}

async fn permit_upload(state: &FilesState, user: Id, channel_id: Id) -> Result<(), HttpError> {
    match state
        .perms
        .check(user, Some(channel_id), Permissions::VIEW_CHANNEL)
    {
        Ok(()) => {}
        Err(AccessError::NotFound | AccessError::Forbidden) => return Err(HttpError::NotFound),
    }
    match state
        .perms
        .check(user, Some(channel_id), Permissions::ATTACH_FILES)
    {
        Ok(()) => {}
        Err(AccessError::Forbidden) => {
            return Err(HttpError::Forbidden("missing attach files"));
        }
        Err(AccessError::NotFound) => return Err(HttpError::NotFound),
    }
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    let channel = channels::get(&mut conn, channel_id)
        .await
        .map_err(|_| HttpError::Internal)?
        .filter(|channel| channel.deleted_at.is_none())
        .ok_or(HttpError::NotFound)?;
    if matches!(
        channel.kind,
        ChannelKind::Text | ChannelKind::Dm | ChannelKind::GroupDm
    ) {
        Ok(())
    } else {
        Err(HttpError::BadRequest("not a text channel"))
    }
}

async fn may_read(state: &FilesState, user: Id, file: &db::Attachment) -> Result<(), HttpError> {
    if let Some(message_id) = file.message_id {
        let mut conn = state
            .perms
            .pool()
            .read()
            .await
            .map_err(|_| HttpError::Internal)?;
        let message = messages::get(&mut conn, message_id)
            .await
            .map_err(|_| HttpError::Internal)?
            .ok_or(HttpError::NotFound)?;
        drop(conn);
        return match state
            .perms
            .check(user, Some(message.channel_id), Permissions::VIEW_CHANNEL)
        {
            Ok(()) => Ok(()),
            Err(_) => Err(HttpError::NotFound),
        };
    }
    if file.uploader_id == user {
        return Ok(());
    }
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    if users::uses_avatar(&mut conn, file.id)
        .await
        .map_err(|_| HttpError::Internal)?
    {
        return Ok(());
    }
    if is_icon(&mut conn, file.id)
        .await
        .map_err(|_| HttpError::Internal)?
    {
        return Ok(());
    }
    Err(HttpError::NotFound)
}

async fn is_icon(conn: &mut sqlx::SqliteConnection, id: Id) -> Result<bool, db::Error> {
    let Some(setting) = settings::get(conn, settings::ICON).await? else {
        return Ok(false);
    };
    Ok(setting.value.parse::<Id>().ok() == Some(id))
}

async fn load_file(state: &FilesState, id: Id) -> Result<db::Attachment, HttpError> {
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    attachments::get(&mut conn, id)
        .await
        .map_err(|_| HttpError::Internal)?
        .ok_or(HttpError::NotFound)
}

async fn serve(
    state: &FilesState,
    file: &db::Attachment,
    headers: &HeaderMap,
) -> Result<Response, HttpError> {
    let etag = format!("\"{}\"", hex(&file.sha256));
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| etag_matches(value, &etag))
    {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .body(Body::empty())
            .map_err(|_| HttpError::Internal);
    }
    let bytes = match state.files.read(&file.storage_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(HttpError::NotFound);
        }
        Err(error) => {
            tracing::debug!(%error, "attachment read failed");
            return Err(HttpError::Internal);
        }
    };
    let disposition = content_disposition(&file.filename, inline_type(&file.mime));
    Response::builder()
        .header(header::CONTENT_TYPE, &file.mime)
        .header(header::ETAG, etag)
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, super::hardening::CSP)
        .header(header::CACHE_CONTROL, "private, no-cache")
        .body(Body::from(bytes))
        .map_err(|_| HttpError::Internal)
}

pub(crate) async fn read_upload(
    multipart: &mut Multipart,
    max_bytes: u64,
) -> Result<(String, String, Vec<u8>), HttpError> {
    let max = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut channel_id = None;
    let mut file = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| HttpError::BadRequest("upload is not valid multipart"))?
    {
        let name = field.name().unwrap_or("").to_owned();
        let filename = field.file_name().map(str::to_owned);
        match name.as_str() {
            "channel_id" => {
                channel_id = Some(read_channel(&mut field).await?);
            }
            "file" => {
                if file.is_some() {
                    return Err(HttpError::BadRequest("one file per upload"));
                }
                let bytes = read_limited(&mut field, max).await?;
                file = Some((filename.unwrap_or_else(|| "file".to_owned()), bytes));
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }
    let channel_id = channel_id.ok_or(HttpError::BadRequest("channel id is required"))?;
    let (filename, bytes) = file.ok_or(HttpError::BadRequest("file is required"))?;
    if bytes.is_empty() {
        return Err(HttpError::BadRequest("file is empty"));
    }
    Ok((channel_id, filename, bytes))
}

async fn read_channel(
    field: &mut axum::extract::multipart::Field<'_>,
) -> Result<String, HttpError> {
    let bytes = match read_limited(field, 64).await {
        Ok(bytes) => bytes,
        Err(HttpError::Payload(_)) => {
            return Err(HttpError::BadRequest("channel id must be a ULID"));
        }
        Err(error) => return Err(error),
    };
    String::from_utf8(bytes).map_err(|_| HttpError::BadRequest("channel id must be a ULID"))
}

async fn read_limited(
    field: &mut axum::extract::multipart::Field<'_>,
    max: usize,
) -> Result<Vec<u8>, HttpError> {
    let mut buf = Vec::new();
    loop {
        let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| HttpError::BadRequest("upload is not valid multipart"))?
        else {
            break;
        };
        let next = buf.len().saturating_add(chunk.len());
        if next > max {
            return Err(HttpError::Payload("file is too large"));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

async fn caller(auth: &Auth, headers: &HeaderMap) -> Result<Id, HttpError> {
    let Some(token) = bearer(headers) else {
        return Err(HttpError::Unauthorized);
    };
    match auth.authenticate_bearer(token).await {
        Ok(authed) => Ok(authed.user.id),
        Err(
            crate::auth::Error::Unauthenticated
            | crate::auth::Error::Disabled
            | crate::auth::Error::Banned { .. },
        ) => Err(HttpError::Unauthorized),
        Err(_) => Err(HttpError::Internal),
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    if token.is_empty() { None } else { Some(token) }
}

fn parse_id(text: &str) -> Result<Id, HttpError> {
    text.parse()
        .map_err(|_| HttpError::BadRequest("id must be a ULID"))
}

fn save_error(error: SaveError) -> HttpError {
    match error {
        SaveError::Empty => HttpError::BadRequest("file is empty"),
        SaveError::TooLarge => HttpError::Payload("file is too large"),
        SaveError::Quota => HttpError::Quota("daily upload quota exceeded"),
        SaveError::RejectedType => HttpError::Unsupported("file type is not allowed"),
        SaveError::Io(error) => {
            tracing::debug!(%error, "attachment upload failed");
            HttpError::Internal
        }
        SaveError::Database(error) => {
            tracing::debug!(%error, "attachment upload failed");
            HttpError::Internal
        }
    }
}

fn saved_json(saved: &Saved, url: &str) -> Value {
    let mut value = Map::new();
    value.insert("attachmentId".into(), json!(b64(&saved.id.as_bytes())));
    value.insert("url".into(), json!(url));
    value.insert("mime".into(), json!(saved.mime));
    value.insert("size".into(), json!(saved.size.to_string()));
    if let Some(width) = saved.width {
        value.insert("width".into(), json!(width));
    }
    if let Some(height) = saved.height {
        value.insert("height".into(), json!(height));
    }
    Value::Object(value)
}

fn inline_type(mime: &str) -> bool {
    mime.starts_with("image/") || mime.starts_with("video/")
}

fn content_disposition(filename: &str, inline: bool) -> String {
    let kind = if inline { "inline" } else { "attachment" };
    let mut quoted = String::new();
    let mut star = String::new();
    for ch in filename.chars() {
        if ch.is_ascii() && ch != '"' && ch != '\\' && !ch.is_control() {
            quoted.push(ch);
        }
        let mut buf = [0; 4];
        for byte in ch.encode_utf8(&mut buf).bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    star.push(char::from(byte));
                }
                _ => {
                    let _ = write!(star, "%{byte:02X}");
                }
            }
        }
    }
    format!("{kind}; filename=\"{quoted}\"; filename*=UTF-8''{star}")
}

fn etag_matches(header: &str, etag: &str) -> bool {
    header.split(',').any(|part| {
        let part = part.trim();
        part == "*" || part == etag
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(crate) enum HttpError {
    Unauthorized,
    NotFound,
    Forbidden(&'static str),
    BadRequest(&'static str),
    Payload(&'static str),
    Unsupported(&'static str),
    Quota(&'static str),
    RateLimited(std::time::Duration),
    Internal,
}

impl HttpError {
    fn parts(self) -> (StatusCode, &'static str) {
        match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found"),
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, message),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Payload(message) => (StatusCode::PAYLOAD_TOO_LARGE, message),
            Self::Unsupported(message) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, message),
            Self::Quota(message) => (StatusCode::TOO_MANY_REQUESTS, message),
            Self::RateLimited(_) => (StatusCode::TOO_MANY_REQUESTS, "rate limited"),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        }
    }

    /// Body text sent with the status.
    pub(crate) fn message(self) -> &'static str {
        self.parts().1
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let retry_after = match self {
            Self::RateLimited(wait) => Some((wait, "rate limited")),
            Self::Quota(message) => Some((std::time::Duration::from_hours(24), message)),
            _ => None,
        };
        if let Some((wait, message)) = retry_after {
            return super::hardening::too_many_requests_with_message(wait, message);
        }
        let (status, message) = self.parts();
        (status, Json(json!({ "error": message }))).into_response()
    }
}
