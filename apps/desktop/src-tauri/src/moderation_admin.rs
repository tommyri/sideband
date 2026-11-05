//! Member moderation bridge. Every mutation is authorized and audited by the server.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
};
use serde::{Deserialize, Serialize};
use ss_client_core::v1;
use ss_core::{Id, Timestamp};
use tauri::State;
use ts_rs::TS;
/// A member's public administrative metadata, excluding session credentials.
#[derive(Deserialize, Serialize, TS)]
#[ts(export, export_to = "../../src/lib/generated/")]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // Independent account state and action capabilities in the wire projection.
pub struct ModerationMember {
    /// User ULID.
    pub id: String,
    /// Login name.
    pub username: String,
    /// Display name.
    pub name: String,
    /// Roles, highest first, including everyone.
    pub roles: Vec<String>,
    /// Account creation timestamp, decimal Unix milliseconds.
    pub joined_at: String,
    /// Most recent session activity, not an exact offline timestamp.
    pub last_session_at: Option<String>,
    /// Disabled account.
    pub disabled: bool,
    /// An active ban exists.
    pub banned: bool,
    /// Permission and hierarchy allow kicking.
    pub can_kick: bool,
    /// Permission and hierarchy allow banning.
    pub can_ban: bool,
}
/// An active ban; expired and lifted entries are omitted.
#[derive(Deserialize, Serialize, TS)]
#[ts(export, export_to = "../../src/lib/generated/")]
#[serde(rename_all = "camelCase")]
pub struct ModerationBan {
    /// Ban ULID and cursor.
    pub id: String,
    /// Target user ULID.
    pub user_id: String,
    /// Target login name.
    pub username: String,
    /// Target display name.
    pub name: String,
    /// Target roles, highest first.
    pub roles: Vec<String>,
    /// Moderator-provided reason.
    pub reason: String,
    /// Decimal Unix milliseconds.
    pub created_at: String,
    /// Expiration; none is permanent.
    pub expires_at: Option<String>,
    /// Moderator login name.
    pub banned_by: String,
    /// Permission and hierarchy allow lifting the ban.
    pub can_unban: bool,
}
/// A bounded page; exactly one collection is populated.
#[derive(Deserialize, Serialize, TS)]
#[ts(export, export_to = "../../src/lib/generated/")]
#[serde(rename_all = "camelCase")]
pub struct ModerationPage {
    /// Member results.
    pub members: Vec<ModerationMember>,
    /// Ban results.
    pub bans: Vec<ModerationBan>,
    /// Pass as `before` for the next page.
    pub next_cursor: Option<String>,
    /// Whether the caller may inspect bans.
    pub can_ban: bool,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ModerationAction {
    kind: String,
    user_id: String,
    reason: String,
    duration_seconds: Option<u32>,
}
fn invalid(message: &str) -> ApiError {
    ApiError::new("invalid_moderation", message)
}
fn expiry(seconds: Option<u32>, now: i64) -> Result<Option<i64>, ApiError> {
    seconds
        .map(|value| {
            if !matches!(value, 3600 | 86400 | 604800 | 2592000) {
                return Err(invalid("Choose a supported ban duration"));
            }
            now.checked_add(i64::from(value) * 1000)
                .ok_or_else(|| invalid("Invalid expiration"))
        })
        .transpose()
}
#[tauri::command]
pub async fn get_moderation_page(
    state: State<'_, AppState>,
    server_id: String,
    bans: bool,
    before: Option<String>,
    query: String,
) -> Result<ModerationPage, ApiError> {
    let server = parse_server_id(&server_id)?;
    let before = before
        .map(|value| value.parse::<Id>().map_err(|_| invalid("Invalid cursor")))
        .transpose()?;
    let session = {
        let clients = state.clients.lock().await;
        let client = clients
            .get(&server)
            .ok_or_else(|| invalid("Server is disconnected"))?;
        client.attachment_session().await.map_err(api_error)?
    };
    let page = session
        .moderation_page(bans, before, &query)
        .await
        .map_err(api_error)?;
    // Both DTOs deliberately share the same bounded public wire representation.
    serde_json::from_value(serde_json::to_value(page).map_err(|_| invalid("Invalid page"))?)
        .map_err(|_| invalid("Invalid page"))
}
#[tauri::command]
pub async fn moderate_member(
    state: State<'_, AppState>,
    server_id: String,
    action: ModerationAction,
) -> Result<(), ApiError> {
    let server = parse_server_id(&server_id)?;
    let user = action
        .user_id
        .parse::<Id>()
        .map_err(|_| invalid("Invalid member"))?;
    if action.reason.chars().count() > 500 {
        return Err(invalid("Reason must be at most 500 characters"));
    }
    let user_id = user.as_bytes().to_vec().into();
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| invalid("Server is disconnected"))?;
    match action.kind.as_str() {
        "kick" => {
            client
                .kick_user(v1::KickUser {
                    user_id,
                    reason: action.reason,
                })
                .await
        }
        "ban" => {
            client
                .ban_user(v1::BanUser {
                    user_id,
                    reason: action.reason,
                    expires_at: expiry(action.duration_seconds, Timestamp::now().as_millis())?,
                })
                .await
        }
        "unban" => client.unban_user(v1::UnbanUser { user_id }).await,
        _ => return Err(invalid("Unknown moderation action")),
    }
    .map_err(api_error)
}
#[cfg(test)]
mod tests {
    use super::expiry;
    #[test]
    fn ban_presets_are_bounded_and_computed_at_submission() {
        assert_eq!(expiry(None, 1000).unwrap(), None);
        for seconds in [3600, 86400, 604800, 2592000] {
            assert_eq!(
                expiry(Some(seconds), 1000).unwrap(),
                Some(1000 + i64::from(seconds) * 1000)
            );
        }
        assert!(expiry(Some(0), 1000).is_err());
        assert!(expiry(Some(60), 1000).is_err());
        assert!(expiry(Some(3600), i64::MAX).is_err());
    }
}
