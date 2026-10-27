//! `POST /api/v1/webhooks/{id}/{token}`.
//!
//! No bearer. A wrong or revoked token is a 404. The thirty-first post in a
//! minute is 429.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use serde_json::{Value, json};
use ss_core::Id;

use crate::handlers::{Error, Webhooks};

/// Webhook routes. Mount them on the public HTTPS listener.
pub fn router(webhooks: Arc<Webhooks>) -> Router {
    Router::new()
        .route("/api/v1/webhooks/{webhook_id}/{token}", post(execute))
        .with_state(webhooks)
}

async fn execute(
    State(webhooks): State<Arc<Webhooks>>,
    Path((webhook_id, token)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<Value>, HttpError> {
    let id = webhook_id.parse::<Id>().map_err(|_| HttpError::NotFound)?;
    let payload = serde_json::from_slice(&body).map_err(|_| HttpError::BadRequest)?;
    match webhooks.execute(id, &token, &payload).await {
        Ok(message) => Ok(Json(json!({ "id": message.id.to_string() }))),
        Err(Error::NotFound(_)) => Err(HttpError::NotFound),
        Err(Error::Invalid(_)) => Err(HttpError::BadRequest),
        Err(Error::RateLimited(_)) => Err(HttpError::RateLimited),
        Err(Error::Forbidden(_) | Error::Database(_)) => Err(HttpError::Internal),
    }
}

enum HttpError {
    NotFound,
    BadRequest,
    RateLimited,
    Internal,
}

impl IntoResponse for HttpError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            Self::NotFound => (StatusCode::NOT_FOUND, "not found"),
            Self::BadRequest => (StatusCode::BAD_REQUEST, "bad request"),
            Self::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate limited"),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
