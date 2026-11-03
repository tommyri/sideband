//! Public `GET /api/v1/server`.
//!
//! No bearer token. Clients use the fingerprint to decide whether a stored
//! pin still matches the leaf the QUIC listener is offering.

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde_json::{Value, json};
use ss_net::tls::Fingerprint;

use crate::db::{self, Pool, settings};

/// Name, version, fingerprint, and registration mode.
pub fn router(pool: Arc<Pool>, fingerprint: Arc<ArcSwap<Option<Fingerprint>>>) -> Router {
    Router::new()
        .route("/api/v1/server", get(server_info))
        .with_state(Arc::new(InfoState { pool, fingerprint }))
}

struct InfoState {
    pool: Arc<Pool>,
    fingerprint: Arc<ArcSwap<Option<Fingerprint>>>,
}

async fn server_info(State(state): State<Arc<InfoState>>) -> Result<Json<Value>, StatusCode> {
    let mut conn = state
        .pool
        .read()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let name = settings::get(&mut conn, settings::NAME)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mode = settings::get(&mut conn, settings::REGISTRATION_MODE)
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
    Ok(Json(json!({
        "name": name,
        "version": env!("CARGO_PKG_VERSION"),
        "fingerprint": fingerprint,
        "registrationMode": registration,
    })))
}
