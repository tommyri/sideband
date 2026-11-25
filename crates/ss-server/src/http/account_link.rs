//! Authenticated account review and explicit profile link.
use crate::auth::{Auth, Authenticated, Error};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::{get, put},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ss_core::profile_link::{AccountReview, LinkAccount};
use std::sync::Arc;

/// These routes act only on the member identified by the current bearer session.
pub fn router(auth: Arc<Auth>) -> Router {
    Router::new()
        .route("/api/v1/me/profile-link", get(review))
        .route("/api/v1/me/profile-link", put(link))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(auth)
}

async fn caller(auth: &Auth, headers: &HeaderMap) -> Result<Authenticated, StatusCode> {
    let token = super::audit::bearer(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    auth.authenticate_bearer(token)
        .await
        .map_err(|error| status(&error))
}

fn status(error: &Error) -> StatusCode {
    match error {
        Error::Unauthenticated => StatusCode::UNAUTHORIZED,
        Error::Disabled | Error::Banned { .. } => StatusCode::FORBIDDEN,
        Error::InvalidProfile => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn review(
    State(auth): State<Arc<Auth>>,
    headers: HeaderMap,
) -> Result<Json<AccountReview>, StatusCode> {
    let account = caller(&auth, &headers).await?;
    let device = headers
        .get("x-sideband-profile-device")
        .and_then(|header| header.to_str().ok())
        .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    auth.review_account(&account, &device)
        .await
        .map(Json)
        .map_err(|error| status(&error))
}

async fn link(
    State(auth): State<Arc<Auth>>,
    headers: HeaderMap,
    Json(proof): Json<LinkAccount>,
) -> Result<StatusCode, StatusCode> {
    let account = caller(&auth, &headers).await?;
    auth.link_account(&account, &proof)
        .await
        .map_err(|error| status(&error))?;
    Ok(StatusCode::NO_CONTENT)
}
