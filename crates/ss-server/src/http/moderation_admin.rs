//! Bounded moderator-only member and active-ban lists. No credentials are projected.
use super::audit::{AuditState, HttpError, caller};
use crate::{auth::Auth, perms::PermissionService};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Connection;
use ss_core::{Id, Permissions, Timestamp};
use std::sync::Arc;
/// Read metadata for the member administration UI.
pub fn router(auth: Arc<Auth>, perms: Arc<PermissionService>) -> Router {
    Router::new()
        .route("/api/v1/moderation/members", get(members))
        .route("/api/v1/moderation/bans", get(bans))
        .with_state(AuditState { auth, perms })
}
#[derive(Deserialize)]
struct Page {
    before: Option<String>,
    q: Option<String>,
}
impl Page {
    fn filter(self) -> Result<(Option<Id>, String), HttpError> {
        let before = self
            .before
            .map(|s| {
                s.parse()
                    .map_err(|_| HttpError::BadRequest("invalid cursor"))
            })
            .transpose()?;
        let q = self.q.unwrap_or_default().trim().to_owned();
        if q.chars().count() > 100 {
            return Err(HttpError::BadRequest(
                "search must be at most 100 characters",
            ));
        }
        Ok((before, q))
    }
}
#[derive(sqlx::FromRow)]
struct MemberRow {
    id: Id,
    username: String,
    name: String,
    joined_at: i64,
    last_session_at: Option<i64>,
    disabled: bool,
    banned: bool,
}
#[derive(sqlx::FromRow)]
struct BanRow {
    id: Id,
    user_id: Id,
    username: String,
    name: String,
    reason: Option<String>,
    created_at: i64,
    expires_at: Option<i64>,
    banned_by: String,
}
async fn role_names(conn: &mut sqlx::SqliteConnection, user: Id) -> Result<Vec<String>, HttpError> {
    sqlx::query_scalar(
        "SELECT name FROM roles WHERE is_default = 1
        OR id IN (SELECT role_id FROM user_roles WHERE user_id = ?)
        ORDER BY position DESC, id DESC",
    )
    .bind(user)
    .fetch_all(conn)
    .await
    .map_err(|_| HttpError::Internal)
}
async fn members(
    State(state): State<AuditState>,
    Query(query): Query<Page>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpError> {
    let user = caller(&state, &headers).await?;
    let kick = state
        .perms
        .check(user, None, Permissions::KICK_MEMBERS)
        .is_ok();
    let ban = state
        .perms
        .check(user, None, Permissions::BAN_MEMBERS)
        .is_ok();
    if !kick && !ban {
        return Err(HttpError::Forbidden);
    }
    let (before, q) = query.filter()?;
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    let mut tx = conn.begin().await.map_err(|_| HttpError::Internal)?;
    let rows = sqlx::query_as::<_, MemberRow>(
        "SELECT u.id, u.username, u.display_name AS name, u.created_at AS joined_at,
         (SELECT MAX(last_seen_at) FROM sessions WHERE user_id = u.id) AS last_session_at,
         u.disabled_at IS NOT NULL AS disabled,
         EXISTS(SELECT 1 FROM bans WHERE user_id = u.id AND lifted_at IS NULL
                AND (expires_at IS NULL OR expires_at > ?)) AS banned
         FROM users u WHERE (? IS NULL OR u.id < ?)
         AND (instr(lower(u.username),lower(?)) > 0 OR instr(lower(u.display_name),lower(?)) > 0)
         ORDER BY u.id DESC LIMIT 51",
    )
    .bind(Timestamp::now().as_millis())
    .bind(before)
    .bind(before)
    .bind(&q)
    .bind(&q)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| HttpError::Internal)?;
    let more = rows.len() > 50;
    let mut entries = Vec::new();
    for row in rows.into_iter().take(50) {
        let allowed = user != row.id && state.perms.can_act_on(user, row.id);
        let roles = role_names(&mut tx, row.id).await?;
        entries.push(json!({
            "id":row.id.to_string(),
            "username":row.username,
            "name":row.name,
            "roles":roles,
            "joinedAt":row.joined_at.to_string(),
            "lastSessionAt":row.last_session_at.map(|t|t.to_string()),
            "disabled":row.disabled,
            "banned":row.banned,
            "canKick":kick&&allowed&&!row.disabled&&!row.banned,
            "canBan":ban&&allowed&&!row.disabled&&!row.banned
        }));
    }
    tx.commit().await.map_err(|_| HttpError::Internal)?;
    let next = more.then(|| entries.last().expect("full page")["id"].clone());
    Ok(Json(
        json!({"members":entries,"bans":[],"nextCursor":next,"canBan":ban}),
    ))
}
async fn bans(
    State(state): State<AuditState>,
    Query(query): Query<Page>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpError> {
    let user = caller(&state, &headers).await?;
    state
        .perms
        .check(user, None, Permissions::BAN_MEMBERS)
        .map_err(|_| HttpError::Forbidden)?;
    let (before, q) = query.filter()?;
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    let mut tx = conn.begin().await.map_err(|_| HttpError::Internal)?;
    let rows = sqlx::query_as::<_, BanRow>(
        "SELECT b.id,b.user_id,u.username,u.display_name AS name,b.reason,
         b.created_at,b.expires_at,moderator.username AS banned_by FROM bans b
         JOIN users u ON u.id=b.user_id JOIN users moderator ON moderator.id=b.banned_by
         WHERE b.lifted_at IS NULL AND (b.expires_at IS NULL OR b.expires_at > ?)
         AND (? IS NULL OR b.id < ?)
         AND (instr(lower(u.username),lower(?)) > 0 OR instr(lower(u.display_name),lower(?)) > 0)
         ORDER BY b.id DESC LIMIT 51",
    )
    .bind(Timestamp::now().as_millis())
    .bind(before)
    .bind(before)
    .bind(&q)
    .bind(&q)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| HttpError::Internal)?;
    let more = rows.len() > 50;
    let mut entries = Vec::new();
    for row in rows.into_iter().take(50) {
        let roles = role_names(&mut tx, row.user_id).await?;
        entries.push(json!({
            "id":row.id.to_string(),
            "userId":row.user_id.to_string(),
            "username":row.username,
            "name":row.name,
            "roles":roles,
            "reason":row.reason.unwrap_or_default(),
            "createdAt":row.created_at.to_string(),
            "expiresAt":row.expires_at.map(|t|t.to_string()),
            "bannedBy":row.banned_by,
            "canUnban":user!=row.user_id&&state.perms.can_act_on(user,row.user_id)
        }));
    }
    tx.commit().await.map_err(|_| HttpError::Internal)?;
    let next = more.then(|| entries.last().expect("full page")["id"].clone());
    Ok(Json(
        json!({"members":[],"bans":entries,"nextCursor":next,"canBan":true}),
    ))
}
