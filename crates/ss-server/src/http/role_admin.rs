//! Authenticated, visible guild channel permission overrides.
use super::audit::{AuditState, HttpError, caller};
use crate::{auth::Auth, db, perms::PermissionService};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::HeaderMap,
    routing::get,
};
use serde_json::{Value, json};
use ss_core::{ChannelKind, Id, Permissions};
use std::sync::Arc;
/// Read overrides without exposing hidden channels or direct conversations.
pub fn router(auth: Arc<Auth>, perms: Arc<PermissionService>) -> Router {
    Router::new()
        .route("/api/v1/channels/{channel_id}/overrides", get(read))
        .with_state(AuditState { auth, perms })
}
async fn read(
    State(state): State<AuditState>,
    Path(channel): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, HttpError> {
    let user = caller(&state, &headers).await?;
    let channel = channel
        .parse::<Id>()
        .map_err(|_| HttpError::BadRequest("invalid channel"))?;
    state
        .perms
        .check(user, None, Permissions::MANAGE_CHANNELS)
        .map_err(|_| HttpError::Forbidden)?;
    state
        .perms
        .check(user, Some(channel), Permissions::VIEW_CHANNEL)
        .map_err(|_| HttpError::Forbidden)?;
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    let row = db::channels::get(&mut conn, channel)
        .await
        .map_err(|_| HttpError::Internal)?
        .ok_or(HttpError::Forbidden)?;
    if row.deleted_at.is_some()
        || !matches!(
            row.kind,
            ChannelKind::Category | ChannelKind::Text | ChannelKind::Voice
        )
    {
        return Err(HttpError::Forbidden);
    }
    let rows = db::channels::overrides(&mut conn, channel)
        .await
        .map_err(|_| HttpError::Internal)?;
    Ok(Json(rows.into_iter().map(|r| json!({"roleId":r.role_id.to_string(),"allow":r.allow.bits().to_string(),"deny":r.deny.bits().to_string()})).collect()))
}
