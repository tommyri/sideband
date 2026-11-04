//! `GET /api/v1/audit-log`.
//!
//! Bearer auth is the session token from `Welcome`. The caller needs
//! `VIEW_AUDIT_LOG`. Rows are newest first.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use ss_core::{Id, Permissions};

use crate::{
    auth::Auth,
    db::{self, audit_log},
    perms::PermissionService,
};

/// Shared auth and permission snapshot.
#[derive(Clone)]
pub struct AuditState {
    /// Session-token lookup.
    pub auth: Arc<Auth>,
    /// `VIEW_AUDIT_LOG` checks.
    pub perms: Arc<PermissionService>,
}

/// Audit routes. Mount them on the public HTTPS listener.
pub fn router(auth: Arc<Auth>, perms: Arc<PermissionService>) -> Router {
    Router::new()
        .route("/api/v1/audit-log", get(page))
        .with_state(AuditState { auth, perms })
}

#[derive(Deserialize)]
struct PageQuery {
    before: Option<String>,
    limit: Option<u32>,
}

async fn page(
    State(state): State<AuditState>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, HttpError> {
    let user = caller(&state, &headers).await?;
    if state
        .perms
        .check(user, None, Permissions::VIEW_AUDIT_LOG)
        .is_err()
    {
        return Err(HttpError::Forbidden);
    }
    let before = query.before.as_deref().map(parse_id).transpose()?;
    let limit = page_limit(query.limit)?;
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    let rows = audit_log::page(&mut conn, before, limit)
        .await
        .map_err(|_| HttpError::Internal)?;
    Ok(Json(rows.iter().map(entry_json).collect()))
}

fn entry_json(entry: &db::AuditEntry) -> Value {
    let details = serde_json::from_str::<Value>(&entry.details)
        .unwrap_or_else(|_| Value::String(entry.details.clone()));
    json!({
        "id": b64(&entry.id),
        "actorId": entry.actor_id.map(|id| b64(&id)),
        "action": entry.action,
        "targetKind": entry.target_kind,
        "targetId": entry.target_id.map(|id| b64(&id)),
        "details": details,
        "createdAt": entry.created_at.as_millis().to_string(),
    })
}

pub(super) async fn caller(state: &AuditState, headers: &HeaderMap) -> Result<Id, HttpError> {
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

fn b64(id: &Id) -> String {
    base64::engine::general_purpose::STANDARD.encode(id.as_bytes())
}

pub(super) enum HttpError {
    Unauthorized,
    Forbidden,
    BadRequest(&'static str),
    Internal,
}

impl IntoResponse for HttpError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
