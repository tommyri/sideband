//! Paginated invite metadata. Plaintext codes and stored hashes are never returned.
use super::audit::{AuditState, HttpError, caller};
use crate::{auth::Auth, db, perms::PermissionService};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use ss_core::{Id, Permissions};
use std::sync::Arc;
/// Managers see all invites; invite creators see only their own.
pub fn router(auth: Arc<Auth>, perms: Arc<PermissionService>) -> Router {
    Router::new()
        .route("/api/v1/invites", get(page))
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
    let manager = state
        .perms
        .check(user, None, Permissions::MANAGE_SERVER)
        .is_ok();
    if !manager
        && state
            .perms
            .check(user, None, Permissions::CREATE_INVITES)
            .is_err()
    {
        return Err(HttpError::Forbidden);
    }
    let before = query
        .before
        .map(|s| {
            s.parse::<Id>()
                .map_err(|_| HttpError::BadRequest("before must be a ULID"))
        })
        .transpose()?;
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(HttpError::BadRequest("limit must be between 1 and 100"));
    }
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    let rows = db::invites::page(&mut conn, (!manager).then_some(user), before, limit)
        .await
        .map_err(|_| HttpError::Internal)?;
    let b64 = |id: Id| base64::engine::general_purpose::STANDARD.encode(id.as_bytes());
    Ok(Json(
        rows.into_iter()
            .map(|i| {
                json!({
                    "id": b64(i.id), "createdBy": b64(i.created_by), "maxUses": i.max_uses,
                    "uses": i.uses, "createdAt": i.created_at.as_millis().to_string(),
                    "expiresAt": i.expires_at.map(|t| t.as_millis().to_string()),
                    "revokedAt": i.revoked_at.map(|t| t.as_millis().to_string())
                })
            })
            .collect(),
    ))
}
