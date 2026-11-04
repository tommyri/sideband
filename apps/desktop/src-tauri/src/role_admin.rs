//! Role administration bridge; the server remains authoritative for every mutation.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
};
use serde::{Deserialize, Serialize};
use ss_client_core::{ConnectionState, v1};
use ss_core::{Id, Permissions};
use tauri::State;
use ts_rs::TS;
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct AdminRole {
    id: String,
    name: String,
    color: Option<u32>,
    permissions: String,
    position: i32,
    is_default: bool,
    editable: bool,
}
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct RoleMember {
    id: String,
    name: String,
    role_ids: Vec<String>,
    editable: bool,
}
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct RoleSnapshot {
    roles: Vec<AdminRole>,
    members: Vec<RoleMember>,
    administrator: bool,
    manage_roles: bool,
    manage_channels: bool,
    supports_v2: bool,
    create_position: Option<i32>,
}
#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct RoleFields {
    name: String,
    color: Option<u32>,
    permissions: String,
}
#[derive(Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum RoleAction {
    Create {
        fields: RoleFields,
    },
    Edit {
        role: String,
        fields: RoleFields,
    },
    Delete {
        role: String,
    },
    Move {
        role: String,
        before: Option<String>,
    },
    Assign {
        member: String,
        roles: Vec<String>,
    },
    Override {
        channel: String,
        role: String,
        allow: String,
        deny: String,
    },
}
fn invalid(message: &str) -> ApiError {
    ApiError::new("invalid_role", message)
}
fn parse(value: &str) -> Result<Id, ApiError> {
    value.parse().map_err(|_| invalid("Invalid identifier"))
}
fn wire(value: &str) -> Result<bytes::Bytes, ApiError> {
    Ok(parse(value)?.as_bytes().to_vec().into())
}
fn display(value: &[u8]) -> Result<String, ApiError> {
    Id::try_from(value)
        .map(|v| v.to_string())
        .map_err(|_| invalid("Invalid identifier"))
}
fn mask(value: &str) -> Result<u64, ApiError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|v| Permissions::from_bits(*v).is_some())
        .ok_or_else(|| invalid("Invalid permissions"))
}
fn fields(value: RoleFields) -> Result<(String, Option<u32>, u64), ApiError> {
    let name = value.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(invalid("Name must be 1 to 100 characters"));
    }
    if value.color.is_some_and(|v| v > 0xFF_FF_FF) {
        return Err(invalid("Invalid color"));
    }
    Ok((name, value.color, mask(&value.permissions)?))
}
fn snapshot(state: &ss_client_core::State) -> Result<RoleSnapshot, ApiError> {
    if state.connection != ConnectionState::Connected {
        return Err(invalid("Server is disconnected"));
    }
    let own = state
        .own_user
        .as_ref()
        .ok_or_else(|| invalid("Missing account"))?;
    let own_id = Id::try_from(own.id.as_ref()).map_err(|_| invalid("Invalid account"))?;
    let assigned = |user: Id| {
        state.roles.values().filter(move |r| {
            r.is_default
                || state
                    .members
                    .get(&user)
                    .is_some_and(|m| m.roles.contains(&r.id))
        })
    };
    let own_roles = assigned(own_id).collect::<Vec<_>>();
    let bits = own_roles
        .iter()
        .fold(0, |bits, role| bits | role.permissions);
    let administrator = bits & Permissions::ADMINISTRATOR.bits() != 0;
    let manage_roles = administrator || bits & Permissions::MANAGE_ROLES.bits() != 0;
    let manage_channels = administrator || bits & Permissions::MANAGE_CHANNELS.bits() != 0;
    if !manage_roles && !manage_channels {
        return Err(invalid(
            "Manage roles or manage channels permission is required",
        ));
    }
    let highest = own_roles
        .iter()
        .map(|r| r.position)
        .max()
        .unwrap_or(i32::MIN);
    let mut roles = state
        .roles
        .values()
        .map(|r| {
            Ok(AdminRole {
                id: display(&r.id)?,
                name: r.name.clone(),
                color: r.color,
                permissions: r.permissions.to_string(),
                position: r.position,
                is_default: r.is_default,
                editable: administrator || (r.position < highest && r.permissions & 1 == 0),
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    roles.sort_by(|a, b| b.position.cmp(&a.position).then_with(|| b.id.cmp(&a.id)));
    let mut members = state
        .members
        .iter()
        .filter_map(|(id, m)| m.user.as_ref().map(|u| (id, m, u)))
        .map(|(id, m, u)| {
            Ok(RoleMember {
                id: id.to_string(),
                name: if u.display_name.is_empty() {
                    u.username.clone()
                } else {
                    format!("{} (@{})", u.display_name, u.username)
                },
                role_ids: m
                    .roles
                    .iter()
                    .map(|r| display(r))
                    .collect::<Result<_, _>>()?,
                editable: manage_roles
                    && (administrator
                        || assigned(*id).map(|r| r.position).max().unwrap_or(i32::MIN) < highest),
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    members.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    Ok(RoleSnapshot {
        roles,
        members,
        administrator,
        manage_roles,
        manage_channels,
        supports_v2: state
            .server
            .as_ref()
            .is_some_and(|s| s.features.iter().any(|f| f == "role-management-v2")),
        create_position: if administrator {
            Some(0)
        } else {
            highest.checked_sub(1)
        },
    })
}
#[tauri::command]
pub async fn role_admin_snapshot(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<RoleSnapshot, ApiError> {
    let server = parse_server_id(&server_id)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| invalid("Server is disconnected"))?;
    snapshot(&client.state().read())
}
#[tauri::command]
pub async fn role_overrides(
    state: State<'_, AppState>,
    server_id: String,
    channel: String,
) -> Result<Vec<ss_client_core::role_admin::ChannelOverride>, ApiError> {
    let server = parse_server_id(&server_id)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| invalid("Server is disconnected"))?;
    client
        .channel_overrides(parse(&channel)?)
        .await
        .map_err(api_error)
}
#[tauri::command]
pub async fn role_admin_action(
    state: State<'_, AppState>,
    server_id: String,
    action: RoleAction,
) -> Result<(), ApiError> {
    perform(&state, &server_id, action).await
}
async fn perform(state: &AppState, server: &str, action: RoleAction) -> Result<(), ApiError> {
    let server = parse_server_id(server)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| invalid("Server is disconnected"))?;
    let policy = snapshot(&client.state().read())?;
    match action {
        RoleAction::Create { fields: input } => {
            let (name, color, permissions) = fields(input)?;
            client
                .create_role(v1::CreateRole {
                    name,
                    color,
                    permissions,
                    position: policy
                        .create_position
                        .ok_or_else(|| invalid("No editable position"))?,
                })
                .await
                .map_err(api_error)?;
        }
        RoleAction::Edit {
            role,
            fields: input,
        } => {
            let (name, color, permissions) = fields(input)?;
            if color.is_none()
                && !policy.supports_v2
                && policy
                    .roles
                    .iter()
                    .any(|r| r.id == role && r.color.is_some())
            {
                return Err(invalid("Update the server to clear role colors"));
            }
            client
                .update_role(v1::UpdateRole {
                    role_id: wire(&role)?,
                    name: Some(name),
                    color,
                    permissions: Some(permissions),
                    clear_color: color.is_none(),
                    position: None,
                })
                .await
                .map_err(api_error)?;
        }
        RoleAction::Delete { role } => client
            .delete_role(v1::DeleteRole {
                role_id: wire(&role)?,
            })
            .await
            .map_err(api_error)?,
        RoleAction::Move { role, before } => {
            if !policy.supports_v2 {
                return Err(invalid("Update the server to reorder roles"));
            }
            client
                .move_role(v1::MoveRole {
                    role_id: wire(&role)?,
                    before_role_id: before.as_deref().map(wire).transpose()?,
                })
                .await
                .map_err(api_error)?;
        }
        RoleAction::Assign { member, roles } => client
            .set_user_roles(v1::SetUserRoles {
                user_id: wire(&member)?,
                role_ids: roles.iter().map(|id| wire(id)).collect::<Result<_, _>>()?,
            })
            .await
            .map_err(api_error)?,
        RoleAction::Override {
            channel,
            role,
            allow,
            deny,
        } => client
            .set_channel_override(v1::SetChannelOverride {
                channel_id: wire(&channel)?,
                role_id: wire(&role)?,
                allow: mask(&allow)?,
                deny: mask(&deny)?,
            })
            .await
            .map_err(api_error)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests;
