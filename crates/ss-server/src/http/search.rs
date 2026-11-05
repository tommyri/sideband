//! Permission-scoped message search. Hidden guild channels and nonmember DMs never enter ranking.
use super::audit::{AuditState, HttpError, caller};
use crate::{
    auth::Auth,
    db::search::{self, Filter},
    perms::PermissionService,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use ss_core::{Id, Permissions};
use std::sync::Arc;
/// Authenticated text search route.
pub fn router(auth: Arc<Auth>, perms: Arc<PermissionService>) -> Router {
    Router::new()
        .route("/api/v1/search", get(search_messages))
        .with_state(AuditState { auth, perms })
}
#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    channel: Option<String>,
    from: Option<String>,
    before: Option<i64>,
    after: Option<i64>,
    has: Option<String>,
    limit: Option<u32>,
}
fn id(value: Option<String>) -> Result<Option<Id>, HttpError> {
    value
        .map(|v| {
            v.parse()
                .map_err(|_| HttpError::BadRequest("invalid identifier"))
        })
        .transpose()
}
impl SearchQuery {
    fn filter(self) -> Result<Filter, HttpError> {
        let query = search::literal_query(&self.q).ok_or(HttpError::BadRequest(
            "query must contain 1 to 16 words and at most 256 characters",
        ))?;
        if self
            .has
            .as_deref()
            .is_some_and(|v| !matches!(v, "link" | "file" | "image"))
        {
            return Err(HttpError::BadRequest("invalid attachment filter"));
        }
        let limit = self.limit.unwrap_or(50);
        if !(1..=100).contains(&limit) {
            return Err(HttpError::BadRequest("limit must be 1 to 100"));
        }
        if self.after.zip(self.before).is_some_and(|(a, b)| a >= b) {
            return Err(HttpError::BadRequest("after must precede before"));
        }
        Ok(Filter {
            query,
            channel: id(self.channel)?,
            author: id(self.from)?,
            before: self.before,
            after: self.after,
            has: self.has,
            limit,
        })
    }
}
async fn search_messages(
    State(state): State<AuditState>,
    Query(query): Query<SearchQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, HttpError> {
    let user = caller(&state, &headers).await?;
    let filter = query.filter()?;
    let visible: Vec<Id> = state
        .perms
        .visible_channels(user)
        .into_iter()
        .filter(|id| filter.channel.is_none_or(|channel| channel == *id))
        .collect();
    let mut conn = state
        .perms
        .pool()
        .read()
        .await
        .map_err(|_| HttpError::Internal)?;
    let rows = search::search(&mut conn, &filter, &visible)
        .await
        .map_err(|_| HttpError::Internal)?;
    // Revocation during the SQL query must not leave a stale readable result.
    Ok(Json(rows.into_iter().filter(|r|state.perms.check(user,Some(r.channel_id),Permissions::VIEW_CHANNEL).is_ok()).map(|r|json!({"id":r.id.to_string(),"channelId":r.channel_id.to_string(),"authorId":r.author_id.map(|id|id.to_string()),"authorName":r.author_name,"createdAt":r.created_at.to_string(),"excerpt":r.excerpt})).collect()))
}
