//! `GET /api/v1/channels/{id}/messages` and `.../around/{message_id}`.
//!
//! Bearer auth is the session token from `Welcome`. A channel the caller
//! cannot view is a 404. Pages are ascending id order, at most 100 messages.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use ss_core::Id;
use ss_proto::v1;

use crate::{
    auth::Auth,
    db::{self, messages},
    handlers::messages::to_proto,
    perms::{AccessError, PermissionService},
};

/// Shared auth and permission snapshot.
#[derive(Clone)]
pub struct HistoryState {
    /// Session-token lookup.
    pub auth: Arc<Auth>,
    /// Channel visibility.
    pub perms: Arc<PermissionService>,
}

/// History routes. Mount them on the public HTTPS listener.
pub fn router(auth: Arc<Auth>, perms: Arc<PermissionService>) -> Router {
    Router::new()
        .route("/api/v1/channels/{channel_id}/messages", get(page))
        .route(
            "/api/v1/channels/{channel_id}/messages/around/{message_id}",
            get(around),
        )
        .with_state(HistoryState { auth, perms })
}

#[derive(Deserialize)]
struct PageQuery {
    before: Option<String>,
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<u32>,
}

async fn page(
    State(state): State<HistoryState>,
    Path(channel_id): Path<String>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, HttpError> {
    let user = caller(&state, &headers).await?;
    let channel_id = parse_id(&channel_id)?;
    visible(&state, user, channel_id)?;
    let before = query.before.as_deref().map(parse_id).transpose()?;
    let limit = page_limit(query.limit)?;
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    let page = messages::page_before(&mut conn, channel_id, before, limit)
        .await
        .map_err(|_| HttpError::Internal)?;
    wires(&mut conn, &page).await
}

async fn around(
    State(state): State<HistoryState>,
    Path((channel_id, message_id)): Path<(String, String)>,
    Query(query): Query<LimitQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, HttpError> {
    let user = caller(&state, &headers).await?;
    let channel_id = parse_id(&channel_id)?;
    let message_id = parse_id(&message_id)?;
    visible(&state, user, channel_id)?;
    let limit = page_limit(query.limit)?;
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    let page = messages::page_around(&mut conn, channel_id, message_id, limit)
        .await
        .map_err(|_| HttpError::Internal)?;
    wires(&mut conn, &page).await
}

async fn wires(
    conn: &mut sqlx::SqliteConnection,
    page: &[db::Message],
) -> Result<Json<Vec<Value>>, HttpError> {
    let mut values = Vec::with_capacity(page.len());
    for message in page {
        let wire = to_proto(conn, message)
            .await
            .map_err(|_| HttpError::Internal)?;
        values.push(message_json(&wire));
    }
    Ok(Json(values))
}

async fn caller(state: &HistoryState, headers: &HeaderMap) -> Result<Id, HttpError> {
    let Some(token) = bearer(headers) else {
        return Err(HttpError::Unauthorized);
    };
    match state.auth.authenticate_bearer(token).await {
        Ok(authed) => Ok(authed.user.id),
        Err(
            crate::auth::Error::Unauthenticated
            | crate::auth::Error::Disabled
            | crate::auth::Error::Banned { .. },
        ) => Err(HttpError::Unauthorized),
        Err(_) => Err(HttpError::Internal),
    }
}

fn visible(state: &HistoryState, user: Id, channel: Id) -> Result<(), HttpError> {
    match state
        .perms
        .check(user, Some(channel), ss_core::Permissions::VIEW_CHANNEL)
    {
        Ok(()) => Ok(()),
        Err(AccessError::NotFound | AccessError::Forbidden) => Err(HttpError::NotFound),
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

fn page_limit(limit: Option<u32>) -> Result<u32, HttpError> {
    match limit.unwrap_or(50) {
        value if value > 100 => Err(HttpError::BadRequest("limit must be at most 100")),
        value => Ok(value),
    }
}

fn message_json(message: &v1::Message) -> Value {
    let mut value = Map::new();
    value.insert("id".into(), json!(b64(&message.id)));
    value.insert("channelId".into(), json!(b64(&message.channel_id)));
    if let Some(author) = &message.author {
        value.insert("author".into(), user_json(author));
    }
    value.insert("content".into(), json!(message.content));
    if let Some(reply) = &message.reply_to {
        value.insert("replyTo".into(), json!(b64(reply)));
    }
    if !message.attachments.is_empty() {
        value.insert(
            "attachments".into(),
            json!(
                message
                    .attachments
                    .iter()
                    .map(attachment_json)
                    .collect::<Vec<_>>()
            ),
        );
    }
    if !message.link_previews.is_empty() {
        value.insert(
            "linkPreviews".into(),
            json!(
                message
                    .link_previews
                    .iter()
                    .map(preview_json)
                    .collect::<Vec<_>>()
            ),
        );
    }
    if !message.mentions.is_empty() {
        value.insert(
            "mentions".into(),
            json!(
                message
                    .mentions
                    .iter()
                    .map(|id| b64(id))
                    .collect::<Vec<_>>()
            ),
        );
    }
    value.insert("createdAt".into(), json!(message.created_at.to_string()));
    if let Some(edited) = message.edited_at {
        value.insert("editedAt".into(), json!(edited.to_string()));
    }
    if let Some(webhook) = &message.webhook_id {
        value.insert("webhookId".into(), json!(b64(webhook)));
    }
    Value::Object(value)
}

fn user_json(user: &v1::User) -> Value {
    let mut value = Map::new();
    value.insert("id".into(), json!(b64(&user.id)));
    value.insert("username".into(), json!(user.username));
    value.insert("displayName".into(), json!(user.display_name));
    if let Some(avatar) = &user.avatar_attachment_id {
        value.insert("avatarAttachmentId".into(), json!(b64(avatar)));
    }
    value.insert("kind".into(), json!(kind_name(user.kind)));
    Value::Object(value)
}

fn preview_json(preview: &v1::LinkPreview) -> Value {
    let mut value = Map::new();
    value.insert("url".into(), json!(preview.url));
    if let Some(title) = &preview.title {
        value.insert("title".into(), json!(title));
    }
    if let Some(description) = &preview.description {
        value.insert("description".into(), json!(description));
    }
    if let Some(image) = &preview.image_url {
        value.insert("imageUrl".into(), json!(image));
    }
    if let Some(site) = &preview.site_name {
        value.insert("siteName".into(), json!(site));
    }
    Value::Object(value)
}

fn attachment_json(file: &v1::Attachment) -> Value {
    let mut value = Map::new();
    value.insert("id".into(), json!(b64(&file.id)));
    value.insert("filename".into(), json!(file.filename));
    value.insert("mime".into(), json!(file.mime));
    value.insert("size".into(), json!(file.size.to_string()));
    if let Some(width) = file.width {
        value.insert("width".into(), json!(width));
    }
    if let Some(height) = file.height {
        value.insert("height".into(), json!(height));
    }
    Value::Object(value)
}

fn kind_name(kind: i32) -> &'static str {
    match v1::UserKind::try_from(kind) {
        Ok(v1::UserKind::Bot) => "USER_KIND_BOT",
        Ok(v1::UserKind::User) => "USER_KIND_USER",
        _ => "USER_KIND_UNSPECIFIED",
    }
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

enum HttpError {
    Unauthorized,
    NotFound,
    BadRequest(&'static str),
    Internal,
}

impl IntoResponse for HttpError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found"),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
