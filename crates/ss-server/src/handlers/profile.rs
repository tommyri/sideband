//! `UpdateProfile`.
//!
//! An empty display name keeps the current one. An empty avatar id clears the
//! picture. A present id must be a file this user uploaded.

use std::sync::Arc;

use ss_core::Id;
use ss_proto::{
    v1::{self, ack, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};

use super::{CommandReply, Error};
use crate::{
    db::{self, EVERYONE_ROLE_ID, UserProfile, attachments, roles, users},
    perms::PermissionService,
    registry::ConnectionRegistry,
};

/// Display names longer than this are rejected.
const MAX_DISPLAY: usize = 64;

/// Profile commands for the authenticated control stream.
pub struct Profiles {
    perms: Arc<PermissionService>,
    registry: Arc<ConnectionRegistry>,
}

impl Profiles {
    pub(crate) fn new(perms: Arc<PermissionService>, registry: Arc<ConnectionRegistry>) -> Self {
        Self { perms, registry }
    }

    /// `Some` when `msg` is `UpdateProfile`.
    pub async fn dispatch(
        &self,
        actor: Id,
        msg: &v1::client_envelope::Msg,
    ) -> Option<Result<CommandReply, Error>> {
        let v1::client_envelope::Msg::UpdateProfile(command) = msg else {
            return None;
        };
        Some(self.update(actor, command).await)
    }

    async fn update(&self, actor: Id, command: &v1::UpdateProfile) -> Result<CommandReply, Error> {
        let mut conn = self.perms.pool().write().await;
        let current = users::get(&mut conn, actor)
            .await?
            .ok_or(Error::NotFound("unknown user"))?;
        let display_name = chosen_name(&current.display_name, &command.display_name)?;
        let avatar_attachment_id = match &command.avatar_attachment_id {
            None => current.avatar_attachment_id,
            Some(bytes) if bytes.is_empty() => None,
            Some(bytes) => {
                let id = id_from_bytes(bytes)
                    .map_err(|_| Error::Invalid("avatar id must be 16 bytes"))?;
                Some(owned_avatar(&mut conn, actor, id).await?)
            }
        };
        let updated = users::set_profile(
            &mut conn,
            actor,
            UserProfile {
                display_name,
                avatar_attachment_id,
            },
        )
        .await?;
        let user = wire_user(&updated);
        let member = v1::Member {
            user: Some(user.clone()),
            roles: role_ids(&mut conn, actor).await?,
            presence: Some(self.registry.presence(actor)),
        };
        Ok(CommandReply {
            ack: ack::Result::User(user),
            events: vec![server_envelope::Msg::MemberUpdated(v1::MemberUpdated {
                member: Some(member),
            })],
        })
    }
}

fn chosen_name(current: &str, requested: &str) -> Result<String, Error> {
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return Ok(current.to_owned());
    }
    if trimmed.chars().count() > MAX_DISPLAY {
        return Err(Error::Invalid("display name is too long"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(Error::Invalid("display name is not valid"));
    }
    Ok(trimmed.to_owned())
}

async fn owned_avatar(conn: &mut sqlx::SqliteConnection, actor: Id, id: Id) -> Result<Id, Error> {
    let file = attachments::get(conn, id)
        .await?
        .ok_or(Error::NotFound("unknown attachment"))?;
    if file.uploader_id != actor {
        return Err(Error::Forbidden("attachment belongs to someone else"));
    }
    Ok(id)
}

async fn role_ids(conn: &mut sqlx::SqliteConnection, user: Id) -> Result<Vec<bytes::Bytes>, Error> {
    let mut assigned = roles::roles_for_user(conn, user).await?;
    if !assigned.iter().any(|role| role.id == EVERYONE_ROLE_ID)
        && let Some(everyone) = roles::get(conn, EVERYONE_ROLE_ID).await?
    {
        assigned.push(everyone);
    }
    assigned.sort_by_key(|role| (role.position, role.id));
    Ok(assigned
        .into_iter()
        .map(|role| id_to_bytes(role.id))
        .collect())
}

fn wire_user(user: &db::User) -> v1::User {
    let kind = match user.kind {
        ss_core::UserKind::Bot => v1::UserKind::Bot,
        _ => v1::UserKind::User,
    };
    v1::User {
        id: id_to_bytes(user.id),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        avatar_attachment_id: user.avatar_attachment_id.map(id_to_bytes),
        kind: i32::from(kind),
    }
}
