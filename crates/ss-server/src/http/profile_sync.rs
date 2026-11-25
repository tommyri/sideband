//! Member-only encrypted profile storage. The authenticated device selects the row.
use super::audit::bearer;
use crate::{
    auth::Auth,
    db::{Pool, settings},
};
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use sqlx::{Connection, SqliteConnection};
use ss_core::Id;
use std::sync::Arc;

const MAX_COPY: usize = 512 * 1024;

#[derive(Clone)]
pub(super) struct Relay {
    auth: Arc<Auth>,
    pub(super) pool: Arc<Pool>,
}

/// Authenticated public-listener routes, enabled by the server manager at runtime.
pub fn router(auth: Arc<Auth>, pool: Arc<Pool>) -> Router {
    let relay = Relay { auth, pool };
    Router::new()
        .route("/api/v1/profile-sync/status", get(status))
        .route("/api/v1/profile-sync/copy", get(read))
        .route("/api/v1/profile-sync/copy", put(write))
        .layer(DefaultBodyLimit::max(MAX_COPY))
        .with_state(relay.clone())
        .merge(super::pairing::router(relay))
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Member {
    pub(super) user: Id,
    pub(super) session: Id,
    pub(super) membership_key: [u8; 32],
}

pub(super) async fn authenticated(
    relay: &Relay,
    headers: &HeaderMap,
) -> Result<Member, StatusCode> {
    let token = bearer(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let auth = relay
        .auth
        .authenticate_bearer(token)
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let encoded = headers
        .get("x-sideband-membership")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() == 43)
        .ok_or(StatusCode::FORBIDDEN)?;
    let membership_key = URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    Ok(Member {
        user: auth.user.id,
        session: auth.session_id,
        membership_key,
    })
}

// Check again inside the transaction that reads/writes the copy. A disabled relay,
// expired session or revoked device must not finish a transfer authorized earlier.
pub(super) async fn authorize(
    conn: &mut SqliteConnection,
    member: &Member,
) -> Result<(), StatusCode> {
    if !settings::offers_profile_sync(conn)
        .await
        .map_err(internal)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    crate::auth::load_person(conn, member.user)
        .await
        .map_err(|_| StatusCode::FORBIDDEN)?;
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(
        SELECT 1 FROM sessions s JOIN profile_devices d ON s.profile_device_id = d.id
        JOIN profile_memberships m ON d.membership_key = m.membership_key
        WHERE s.id = ? AND s.user_id = ? AND m.user_id = s.user_id AND m.membership_key = ?
        AND s.expires_at > ? AND d.revoked_at IS NULL)",
    )
    .bind(member.session)
    .bind(member.user)
    .bind(member.membership_key.as_slice())
    .bind(ss_core::Timestamp::now().as_millis())
    .fetch_one(conn)
    .await
    .map_err(internal)?;
    if active {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

fn internal(_: impl std::fmt::Display) -> StatusCode {
    StatusCode::INTERNAL_SERVER_ERROR
}

async fn status(State(relay): State<Relay>, headers: HeaderMap) -> Result<Response, StatusCode> {
    let member = authenticated(&relay, &headers).await?;
    let mut guard = relay.pool.read().await.map_err(internal)?;
    let mut tx = guard.begin().await.map_err(internal)?;
    authorize(&mut tx, &member).await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "no-store")],
    )
        .into_response())
}

fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
}

async fn stored(conn: &mut SqliteConnection, user: Id) -> Result<Option<Vec<u8>>, StatusCode> {
    sqlx::query_scalar("SELECT ciphertext FROM profile_sync_copies WHERE user_id = ?")
        .bind(user)
        .fetch_optional(conn)
        .await
        .map_err(internal)
}

async fn read(State(relay): State<Relay>, headers: HeaderMap) -> Result<Response, StatusCode> {
    let member = authenticated(&relay, &headers).await?;
    let mut guard = relay.pool.read().await.map_err(internal)?;
    let mut tx = guard.begin().await.map_err(internal)?;
    authorize(&mut tx, &member).await?;
    let Some(copy) = stored(&mut tx, member.user).await? else {
        return Ok((
            StatusCode::NO_CONTENT,
            [(header::CACHE_CONTROL, "no-store")],
        )
            .into_response());
    };
    let tag = etag(&copy);
    Ok((
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::ETAG, tag.as_str()),
        ],
        copy,
    )
        .into_response())
}

async fn write(
    State(relay): State<Relay>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    let member = authenticated(&relay, &headers).await?;
    if body.len() < 29 || body.first() != Some(&1) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut guard = relay.pool.write().await;
    let mut tx = guard.begin().await.map_err(internal)?;
    authorize(&mut tx, &member).await?;
    let current = stored(&mut tx, member.user).await?;
    if current.as_deref() == Some(body.as_ref()) {
        return Ok(StatusCode::NO_CONTENT);
    }
    let matches = match &current {
        None => headers.get(header::IF_NONE_MATCH).is_some_and(|v| v == "*"),
        Some(bytes) => headers
            .get(header::IF_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == etag(bytes)),
    };
    if !matches {
        return Err(StatusCode::PRECONDITION_FAILED);
    }
    sqlx::query(
        "INSERT INTO profile_sync_copies(user_id, ciphertext) VALUES (?, ?)
         ON CONFLICT(user_id) DO UPDATE SET ciphertext = excluded.ciphertext",
    )
    .bind(member.user)
    .bind(body.as_ref())
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}
