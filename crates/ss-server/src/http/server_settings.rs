//! Manager-only server profile with optimistic concurrency and audited updates.
use super::audit::{AuditState, caller};
use crate::{
    auth::Auth,
    db::{self, settings},
    perms::PermissionService,
    storage::Store,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Connection;
use ss_core::{Id, Permissions};
use std::sync::Arc;
const REVISION: &str = "profile_revision";
const MAX_ICON: usize = 2 * 1024 * 1024;
#[derive(Clone)]
struct SettingsState {
    access: AuditState,
    files: Store,
}
/// Settings are distinct from operator configuration and never expose arbitrary DB keys.
pub fn router(auth: Arc<Auth>, perms: Arc<PermissionService>, files: Store) -> Router {
    Router::new()
        .route("/api/v1/server/settings", get(read))
        .route("/api/v1/server/settings", put(update))
        .route("/api/v1/server/icon", post(upload_icon))
        .layer(DefaultBodyLimit::max(MAX_ICON))
        .with_state(SettingsState {
            access: AuditState { auth, perms },
            files,
        })
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Profile {
    name: String,
    description: String,
    icon_id: Option<String>,
    registration_mode: String,
    revision: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    #[serde(flatten)]
    profile: Profile,
    icon_filename: Option<String>,
    icon_mime: Option<String>,
}
struct Failure(StatusCode, &'static str);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
fn internal(_: impl std::fmt::Display) -> Failure {
    Failure(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not access server settings",
    )
}
fn invalid(message: &'static str) -> Failure {
    Failure(StatusCode::BAD_REQUEST, message)
}
async fn manager(state: &SettingsState, headers: &HeaderMap) -> Result<Id, Failure> {
    let user = caller(&state.access, headers)
        .await
        .map_err(|_| Failure(StatusCode::UNAUTHORIZED, "Sign in again"))?;
    state
        .access
        .perms
        .check(user, None, Permissions::MANAGE_SERVER)
        .map_err(|_| {
            Failure(
                StatusCode::FORBIDDEN,
                "Manage server permission is required",
            )
        })?;
    Ok(user)
}
async fn value(
    conn: &mut sqlx::SqliteConnection,
    key: &str,
    default: &str,
) -> Result<String, Failure> {
    Ok(settings::get(conn, key)
        .await
        .map_err(internal)?
        .map_or_else(|| default.into(), |s| s.value))
}
async fn snapshot(conn: &mut sqlx::SqliteConnection) -> Result<Snapshot, Failure> {
    let icon = value(conn, settings::ICON, "").await?;
    let file = if icon.is_empty() {
        None
    } else {
        let id = icon.parse::<Id>().map_err(internal)?;
        db::attachments::get(conn, id).await.map_err(internal)?
    };
    Ok(Snapshot {
        profile: Profile {
            name: value(conn, settings::NAME, "Sideband").await?,
            description: value(conn, settings::DESCRIPTION, "").await?,
            icon_id: (!icon.is_empty()).then_some(icon),
            registration_mode: value(conn, settings::REGISTRATION_MODE, "invite").await?,
            revision: value(conn, REVISION, "0").await?,
        },
        icon_filename: file.as_ref().map(|f| f.filename.clone()),
        icon_mime: file.as_ref().map(|f| f.mime.clone()),
    })
}
async fn read(
    State(state): State<SettingsState>,
    headers: HeaderMap,
) -> Result<Json<Snapshot>, Failure> {
    let _ = manager(&state, &headers).await?;
    let mut conn = state.access.perms.pool().read().await.map_err(internal)?;
    let mut tx = conn.begin().await.map_err(internal)?;
    let result = snapshot(&mut tx).await?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(result))
}
async fn update(
    State(state): State<SettingsState>,
    headers: HeaderMap,
    Json(mut input): Json<Profile>,
) -> Result<Json<Snapshot>, Failure> {
    let user = manager(&state, &headers).await?;
    input.name = crate::text::name(&input.name, 100)
        .map_err(|_| invalid("Name must be 1 to 100 visible characters"))?;
    if input.description.chars().count() > 2048 {
        return Err(invalid("Description must be at most 2048 characters"));
    }
    if !matches!(input.registration_mode.as_str(), "invite" | "closed") {
        return Err(invalid("Choose invite-only or closed registration"));
    }
    let mut guard = state.access.perms.pool().write().await;
    let mut tx = guard.begin().await.map_err(internal)?;
    state
        .access
        .perms
        .check(user, None, Permissions::MANAGE_SERVER)
        .map_err(|_| {
            Failure(
                StatusCode::FORBIDDEN,
                "Manage server permission is required",
            )
        })?;
    let current = snapshot(&mut tx).await?;
    if current.profile.revision != input.revision {
        return Err(Failure(
            StatusCode::CONFLICT,
            "Settings changed. Reload before saving.",
        ));
    }
    if let Some(icon) = &input.icon_id {
        let id = icon
            .parse::<Id>()
            .map_err(|_| invalid("Invalid icon identifier"))?;
        let file = db::attachments::get(&mut tx, id)
            .await
            .map_err(internal)?
            .ok_or_else(|| invalid("Icon no longer exists"))?;
        if current.profile.icon_id.as_ref() != Some(icon) && file.uploader_id != user {
            return Err(Failure(
                StatusCode::FORBIDDEN,
                "Upload your own server icon",
            ));
        }
        if file.message_id.is_some()
            || !matches!(
                file.mime.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            )
            || file.size > 2 * 1024 * 1024
            || file.width.is_none_or(|n| !(1..=4096).contains(&n))
            || file.height.is_none_or(|n| !(1..=4096).contains(&n))
        {
            return Err(invalid("Choose a PNG, JPEG or WebP icon up to 2 MiB"));
        }
    }
    let revision = Id::generate().to_string();
    for (key, val) in [
        (settings::NAME, input.name.as_str()),
        (settings::DESCRIPTION, input.description.as_str()),
        (settings::ICON, input.icon_id.as_deref().unwrap_or("")),
        (
            settings::REGISTRATION_MODE,
            input.registration_mode.as_str(),
        ),
        (REVISION, revision.as_str()),
    ] {
        settings::set(&mut tx, key, val).await.map_err(internal)?;
    }
    db::audit_log::insert(&mut tx,db::NewAuditEntry {id:Id::generate(),actor_id:Some(user),action:"server.update".into(),target_kind:"server".into(),target_id:None,details:json!({"registrationMode":input.registration_mode,"iconChanged":input.icon_id!=current.profile.icon_id}).to_string()}).await.map_err(internal)?;
    let result = snapshot(&mut tx).await?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(result))
}
async fn upload_icon(
    State(state): State<SettingsState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, Failure> {
    let user = manager(&state, &headers).await?;
    let mime = infer::get(&body).map(|t| t.mime_type());
    if !matches!(mime, Some("image/png" | "image/jpeg" | "image/webp")) {
        return Err(invalid("Choose a PNG, JPEG or WebP image"));
    }
    let image = image::ImageReader::new(std::io::Cursor::new(&body))
        .with_guessed_format()
        .map_err(|_| invalid("Invalid image"))?;
    let (width, height) = image
        .into_dimensions()
        .map_err(|_| invalid("Invalid image"))?;
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(invalid("Icon dimensions must be at most 4096 by 4096"));
    }
    let file = state
        .files
        .save(user, "server-icon", &body)
        .await
        .map_err(|_| invalid("Icon upload failed; check the file and upload quota"))?;
    Ok(Json(json!({"iconId":file.id.to_string()})))
}
