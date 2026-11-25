//! Public `GET /api/v1/server`.
//!
//! No bearer token. Clients use the fingerprint to decide whether a stored
//! pin still matches the leaf the QUIC listener is offering.

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use sqlx::Connection;
use ss_net::tls::Fingerprint;

use crate::db::{self, Pool, settings};

/// Public identity, registration policy and supported profile capabilities.
pub fn router(pool: Arc<Pool>, fingerprint: Arc<ArcSwap<Option<Fingerprint>>>) -> Router {
    Router::new()
        .route("/api/v1/server", get(server_info))
        .with_state(Arc::new(InfoState { pool, fingerprint }))
}

struct InfoState {
    pool: Arc<Pool>,
    fingerprint: Arc<ArcSwap<Option<Fingerprint>>>,
}

async fn server_info(State(state): State<Arc<InfoState>>) -> Result<Response, StatusCode> {
    let mut conn = state
        .pool
        .read()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut tx = conn
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let name = settings::get(&mut tx, settings::NAME)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mode = settings::get(&mut tx, settings::REGISTRATION_MODE)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let fingerprint = (**state.fingerprint.load())
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let name = name.map_or_else(|| "Sideband".to_owned(), |setting| setting.value);
    let registration = mode.map_or_else(
        || db::REGISTRATION_INVITE.to_owned(),
        |setting| setting.value,
    );
    let logo = settings::public_icon(&mut tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut capabilities = vec!["profileSignIn"];
    if settings::offers_profile_sync(&mut tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        capabilities.push("profileSync");
    }
    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut metadata = json!({
        "name": name,
        "capabilities": capabilities,
        "fingerprint": fingerprint,
        "registrationMode": registration,
    });
    if logo.is_some() {
        metadata["iconUrl"] = Value::from("/api/v1/server/icon");
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(metadata)).into_response())
}
