//! Permission-aware invite presentation and commands. Codes are never retained here.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
};
use serde::{Deserialize, Serialize};
use ss_client_core::v1;
use ss_core::Id;
use tauri::State;
use ts_rs::TS;
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct InviteView {
    id: String,
    created_by: String,
    max_uses: Option<u32>,
    uses: u32,
    #[ts(type = "number")]
    created_at: i64,
    #[ts(type = "number | null")]
    expires_at: Option<i64>,
    #[ts(type = "number | null")]
    revoked_at: Option<i64>,
}
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct CreatedInvite {
    id: String,
    code: String,
}
#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct CreateInviteInput {
    server_id: String,
    max_uses: Option<u32>,
    expires_in_hours: Option<u32>,
}
fn error(message: &str) -> ApiError {
    ApiError::new("invalid_invite", message)
}
fn display(bytes: &[u8]) -> Result<String, ApiError> {
    Id::try_from(bytes)
        .map(|id| id.to_string())
        .map_err(|_| error("Invalid invite identifier"))
}
fn project(invite: v1::Invite) -> Result<InviteView, ApiError> {
    Ok(InviteView {
        id: display(&invite.id)?,
        created_by: display(&invite.created_by)?,
        max_uses: invite.max_uses,
        uses: invite.uses,
        created_at: invite.created_at,
        expires_at: invite.expires_at,
        revoked_at: invite.revoked_at,
    })
}
#[tauri::command]
pub async fn invite_page(
    state: State<'_, AppState>,
    server_id: String,
    before: Option<String>,
) -> Result<Vec<InviteView>, ApiError> {
    let server = parse_server_id(&server_id)?;
    let before = before
        .map(|s| s.parse::<Id>().map_err(|_| error("Invalid page cursor")))
        .transpose()?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| error("Server is disconnected"))?;
    client
        .list_invites(before)
        .await
        .map_err(api_error)?
        .into_iter()
        .map(project)
        .collect()
}
fn create_command(input: &CreateInviteInput) -> Result<v1::CreateInvite, ApiError> {
    if input.max_uses == Some(0) {
        return Err(error("Maximum uses must be at least 1"));
    }
    let expires_at = input
        .expires_in_hours
        .map(|hours| {
            if !(1..=8760).contains(&hours) {
                return Err(error("Expiry must be between 1 hour and 1 year"));
            }
            Ok(ss_core::Timestamp::now().as_millis() + i64::from(hours) * 3_600_000)
        })
        .transpose()?;
    Ok(v1::CreateInvite {
        max_uses: input.max_uses,
        expires_at,
    })
}
#[tauri::command]
pub async fn create_server_invite(
    state: State<'_, AppState>,
    input: CreateInviteInput,
) -> Result<CreatedInvite, ApiError> {
    let server = parse_server_id(&input.server_id)?;
    let command = create_command(&input)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| error("Server is disconnected"))?;
    let invite = client.create_invite(command).await.map_err(api_error)?;
    Ok(CreatedInvite {
        id: display(&invite.id)?,
        code: invite.code,
    })
}
#[tauri::command]
pub async fn revoke_server_invite(
    state: State<'_, AppState>,
    server_id: String,
    invite_id: String,
) -> Result<(), ApiError> {
    let server = parse_server_id(&server_id)?;
    let invite = invite_id
        .parse::<Id>()
        .map_err(|_| error("Invalid invite identifier"))?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| error("Server is disconnected"))?;
    client
        .revoke_invite(v1::RevokeInvite {
            invite_id: invite.as_bytes().to_vec().into(),
        })
        .await
        .map_err(api_error)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_use_and_expiry_limits() {
        let mut input = CreateInviteInput {
            server_id: "server".into(),
            max_uses: Some(1),
            expires_in_hours: Some(24),
        };
        let command = create_command(&input).unwrap();
        assert!(command.expires_at.unwrap() > ss_core::Timestamp::now().as_millis());
        input.max_uses = Some(0);
        assert!(create_command(&input).is_err());
        input.max_uses = None;
        input.expires_in_hours = Some(0);
        assert!(create_command(&input).is_err());
        input.expires_in_hours = Some(8761);
        assert!(create_command(&input).is_err());
        input.expires_in_hours = None;
        assert_eq!(create_command(&input).unwrap().expires_at, None);
    }
    #[test]
    fn metadata_projection_discards_plaintext_codes() {
        let row = project(v1::Invite {
            id: Id::generate().as_bytes().to_vec().into(),
            created_by: Id::generate().as_bytes().to_vec().into(),
            code: "secret".into(),
            ..Default::default()
        })
        .unwrap();
        let value = serde_json::to_value(row).unwrap();
        assert!(value.get("code").is_none());
        assert!(!value.to_string().contains("secret"));
    }
}
