//! `POST /api/v1/webhooks/{id}/{token}`.
//!
//! No bearer. The body must be `application/json` (415 otherwise) and at
//! most 64 KiB. A wrong or revoked token is a 404. The thirty-first post in a
//! minute is 429 with `Retry-After`.

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use ss_core::Id;

use super::hardening::{MAX_BODY, has_content_type, too_many_requests};
use crate::handlers::{Error, Webhooks};

/// Webhook routes. Mount them on the public HTTPS listener.
pub fn router(webhooks: Arc<Webhooks>) -> Router {
    Router::new()
        .route("/api/v1/webhooks/{webhook_id}/{token}", post(execute))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(webhooks)
}

async fn execute(
    State(webhooks): State<Arc<Webhooks>>,
    Path((webhook_id, token)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, HttpError> {
    let id = webhook_id.parse::<Id>().map_err(|_| HttpError::NotFound)?;
    if !has_content_type(&headers, "application/json") {
        return Err(HttpError::Unsupported);
    }
    let payload = serde_json::from_slice(&body).map_err(|_| HttpError::BadRequest)?;
    match webhooks.execute(id, &token, &payload).await {
        Ok(message) => Ok(Json(json!({ "id": message.id.to_string() }))),
        Err(Error::NotFound(_)) => Err(HttpError::NotFound),
        Err(Error::Invalid(_)) => Err(HttpError::BadRequest),
        Err(Error::RateLimited(_, wait)) => Err(HttpError::RateLimited(wait)),
        Err(Error::Forbidden(_) | Error::Database(_)) => Err(HttpError::Internal),
    }
}

enum HttpError {
    NotFound,
    BadRequest,
    Unsupported,
    RateLimited(Duration),
    Internal,
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::NotFound => (StatusCode::NOT_FOUND, "not found"),
            Self::BadRequest => (StatusCode::BAD_REQUEST, "bad request"),
            Self::Unsupported => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "content type must be application/json",
            ),
            Self::RateLimited(wait) => return too_many_requests(wait),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
