//! `CreateRole`, `UpdateRole`, `DeleteRole`, `SetUserRoles`, and `SetChannelOverride`.
//!
//! Role commands require `MANAGE_ROLES`. Channel overrides require
//! `MANAGE_CHANNELS`. A non-admin can only touch a role positioned strictly
//! below their highest role, and cannot grant `ADMINISTRATOR`.

use std::{collections::HashSet, sync::Arc};

use sqlx::Connection;
use ss_core::{Id, Permissions};
use ss_proto::{
    v1::{self, ErrorCode, ack, server_envelope},
    validate::{id_from_bytes, id_to_bytes, permissions_from_bits},
};

use crate::{
    db::{
        self, EVERYONE_ROLE_ID, NewAuditEntry, NewRole, RoleUpdate, audit_log, channels, roles,
        users,
    },
    perms::PermissionService,
};

/// Ack body plus the event to send after it. Override changes have no event.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandReply {
    /// Payload for [`ss_proto::v1::ServerEnvelope::ack`].
    pub ack: ack::Result,
    /// Event frame. The connection sends it with request id 0.
    pub event: Option<server_envelope::Msg>,
}

/// A role command the client can be told about.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The actor lacks the bit, the hierarchy, or the administrator grant.
    #[error("{0}")]
    Forbidden(&'static str),
    /// The user, role, or channel does not exist.
    #[error("{0}")]
    NotFound(&'static str),
    /// The name, id, or permission bits are not valid.
    #[error("{0}")]
    Invalid(&'static str),
    /// SQLite failed. The wire message stays generic.
    #[error(transparent)]
    Database(#[from] db::Error),
}

impl Error {
    /// Protocol code for this failure.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::Forbidden(_) => ErrorCode::Forbidden,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Invalid(_) => ErrorCode::Invalid,
            Self::Database(_) => ErrorCode::Internal,
        }
    }

    /// Message safe to send to the peer.
    pub(crate) fn wire_message(&self) -> String {
        match self {
            Self::Database(_) => "internal error".to_owned(),
            other => other.to_string(),
        }
    }
}

/// Role and override commands. Mutations reload [`PermissionService`].
pub struct Roles {
    perms: Arc<PermissionService>,
}

impl Roles {
    /// Shares `perms` with the caller so a reload is visible there too.
    #[must_use]
    pub fn new(perms: Arc<PermissionService>) -> Self {
        Self { perms }
    }

    /// Loads a permission snapshot, then serves role commands against it.
    pub async fn open(pool: Arc<db::Pool>) -> Result<Self, db::Error> {
        Ok(Self::new(Arc::new(PermissionService::open(pool).await?)))
    }

    /// Dispatches a role command. Other messages return `None`.
    pub async fn dispatch(
        &self,
        actor: Id,
        msg: &v1::client_envelope::Msg,
    ) -> Option<Result<CommandReply, Error>> {
        match msg {
            v1::client_envelope::Msg::CreateRole(command) => {
                Some(self.create(actor, command).await)
            }
            v1::client_envelope::Msg::UpdateRole(command) => {
                Some(self.update(actor, command).await)
            }
            v1::client_envelope::Msg::DeleteRole(command) => {
                Some(self.delete(actor, command).await)
            }
            v1::client_envelope::Msg::SetUserRoles(command) => {
                Some(self.set_user_roles(actor, command).await)
            }
            v1::client_envelope::Msg::SetChannelOverride(command) => {
                Some(self.set_channel_override(actor, command).await)
            }
            _ => None,
        }
    }

    /// Creates a role at the requested position. Positions are not compacted.
    pub async fn create(&self, actor: Id, command: &v1::CreateRole) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::MANAGE_ROLES)?;
        let name = role_name(&command.name)?.to_owned();
        let permissions = bits(command.permissions)?;
        let position = i64::from(command.position);
        self.ensure_position(actor, position)?;
        self.ensure_admin_bits(actor, permissions)?;
        let role = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let role = roles::insert(
                &mut tx,
                NewRole {
                    id: Id::generate(),
                    name,
                    color: command.color.map(i64::from),
                    permissions,
                    position,
                },
            )
            .await?;
            audit(
                &mut tx,
                actor,
                "role.create",
                "role",
                Some(role.id),
                &format!(r#"{{"position":{position}}}"#),
            )
            .await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.reload().await?;
            role
        };
        tracing::info!(%actor, role_id = %role.id, "role created");
        let wire = wire_role(&role)?;
        Ok(CommandReply {
            ack: ack::Result::Role(wire.clone()),
            event: Some(server_envelope::Msg::RoleCreated(v1::RoleCreated {
                role: Some(wire),
            })),
        })
    }

    /// Patches a role. Absent fields stay as stored.
    pub async fn update(&self, actor: Id, command: &v1::UpdateRole) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::MANAGE_ROLES)?;
        let id = required_id(&command.role_id)?;
        if let Some(name) = &command.name {
            role_name(name)?;
        }
        if let Some(value) = command.permissions {
            bits(value)?;
        }
        let role = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let current = roles::get(&mut tx, id)
                .await?
                .ok_or(Error::NotFound("role not found"))?;
            let name = match &command.name {
                Some(name) => role_name(name)?.to_owned(),
                None => current.name.clone(),
            };
            let permissions = match command.permissions {
                Some(value) => bits(value)?,
                None => current.permissions,
            };
            let position = command.position.map_or(current.position, i64::from);
            self.ensure_can_edit(actor, &current)?;
            self.ensure_position(actor, position)?;
            self.ensure_admin_bits(actor, permissions)?;
            let role = roles::update(
                &mut tx,
                id,
                RoleUpdate {
                    name,
                    color: command
                        .color
                        .map_or(current.color, |color| Some(i64::from(color))),
                    permissions,
                    position,
                },
            )
            .await?;
            audit(
                &mut tx,
                actor,
                "role.update",
                "role",
                Some(role.id),
                &format!(r#"{{"position":{position}}}"#),
            )
            .await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.reload().await?;
            role
        };
        tracing::info!(%actor, role_id = %role.id, "role updated");
        let wire = wire_role(&role)?;
        Ok(CommandReply {
            ack: ack::Result::Role(wire.clone()),
            event: Some(server_envelope::Msg::RoleUpdated(v1::RoleUpdated {
                role: Some(wire),
            })),
        })
    }

    /// Deletes a role, clears its grants and overrides, then compacts positions.
    pub async fn delete(&self, actor: Id, command: &v1::DeleteRole) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::MANAGE_ROLES)?;
        let id = required_id(&command.role_id)?;
        let role = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let role = roles::get(&mut tx, id)
                .await?
                .ok_or(Error::NotFound("role not found"))?;
            if role.is_default || role.id == EVERYONE_ROLE_ID {
                return Err(Error::Forbidden("cannot delete @everyone"));
            }
            self.ensure_can_edit(actor, &role)?;
            roles::clear_assignments(&mut tx, id).await?;
            channels::clear_role_overrides(&mut tx, id).await?;
            roles::delete(&mut tx, id).await?;
            compact(&mut tx).await?;
            audit(&mut tx, actor, "role.delete", "role", Some(id), "{}").await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.reload().await?;
            role
        };
        tracing::info!(%actor, role_id = %role.id, "role deleted");
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            event: Some(server_envelope::Msg::RoleDeleted(v1::RoleDeleted {
                role: Some(wire_role(&role)?),
            })),
        })
    }

    /// Replaces `command.user_id`'s stored grants. `@everyone` still applies when omitted.
    pub async fn set_user_roles(
        &self,
        actor: Id,
        command: &v1::SetUserRoles,
    ) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::MANAGE_ROLES)?;
        let target = required_id(&command.user_id)?;
        let desired = unique_ids(&command.role_ids)?;
        if !self.perms.can_act_on(actor, target) {
            return Err(Error::Forbidden("cannot act on that member"));
        }
        let stored = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            if users::get(&mut tx, target).await?.is_none() {
                return Err(Error::NotFound("user not found"));
            }
            let current = roles::roles_for_user(&mut tx, target).await?;
            let current_ids: HashSet<Id> = current.iter().map(|role| role.id).collect();
            let desired_set: HashSet<Id> = desired.iter().copied().collect();
            let changed: Vec<Id> = current_ids
                .symmetric_difference(&desired_set)
                .copied()
                .collect();
            let mut loaded = Vec::with_capacity(changed.len());
            for id in changed {
                let role = roles::get(&mut tx, id)
                    .await?
                    .ok_or(Error::NotFound("role not found"))?;
                loaded.push(role);
            }
            for role in &loaded {
                self.ensure_can_edit(actor, role)?;
            }
            for id in current_ids.difference(&desired_set) {
                roles::unassign(&mut tx, target, *id).await?;
            }
            for id in desired.iter().filter(|id| !current_ids.contains(id)) {
                roles::assign(&mut tx, target, *id).await?;
            }
            let count = desired.len();
            audit(
                &mut tx,
                actor,
                "member.roles.set",
                "user",
                Some(target),
                &format!(r#"{{"count":{count}}}"#),
            )
            .await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.reload().await?;
            desired
        };
        tracing::info!(%actor, %target, "member roles updated");
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            event: Some(server_envelope::Msg::MemberRolesUpdated(
                v1::MemberRolesUpdated {
                    user_id: id_to_bytes(target),
                    role_ids: stored.iter().copied().map(id_to_bytes).collect(),
                },
            )),
        })
    }

    /// Sets one role's allow and deny masks in one channel.
    ///
    /// This requires `MANAGE_CHANNELS`, the bit the data model assigns to
    /// overrides, not `MANAGE_ROLES`.
    pub async fn set_channel_override(
        &self,
        actor: Id,
        command: &v1::SetChannelOverride,
    ) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::MANAGE_CHANNELS)?;
        let channel_id = required_id(&command.channel_id)?;
        let role_id = required_id(&command.role_id)?;
        let allow = bits(command.allow)?;
        let deny = bits(command.deny)?;
        {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let channel = channels::get(&mut tx, channel_id)
                .await?
                .ok_or(Error::NotFound("channel not found"))?;
            if channel.deleted_at.is_some() {
                return Err(Error::NotFound("channel not found"));
            }
            let role = roles::get(&mut tx, role_id)
                .await?
                .ok_or(Error::NotFound("role not found"))?;
            self.ensure_can_edit(actor, &role)?;
            channels::set_override(&mut tx, channel_id, role_id, allow, deny).await?;
            let deny_bits = deny.bits();
            audit(
                &mut tx,
                actor,
                "channel.override.set",
                "channel",
                Some(channel_id),
                &format!(r#"{{"deny":{deny_bits}}}"#),
            )
            .await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.reload().await?;
        }
        tracing::info!(%actor, %channel_id, %role_id, "channel override set");
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            event: None,
        })
    }

    fn require(&self, actor: Id, permission: Permissions) -> Result<(), Error> {
        self.perms
            .check(actor, None, permission)
            .map_err(|_| Error::Forbidden("missing permission"))
    }

    fn ensure_position(&self, actor: Id, position: i64) -> Result<(), Error> {
        if self.perms.is_administrator(actor) {
            return Ok(());
        }
        if Permissions::hierarchy_allows(self.perms.highest_role_position(actor), position) {
            Ok(())
        } else {
            Err(Error::Forbidden("role is not below your highest role"))
        }
    }

    fn ensure_admin_bits(&self, actor: Id, permissions: Permissions) -> Result<(), Error> {
        if permissions.contains(Permissions::ADMINISTRATOR) && !self.perms.is_administrator(actor) {
            Err(Error::Forbidden("cannot grant administrator"))
        } else {
            Ok(())
        }
    }

    fn ensure_can_edit(&self, actor: Id, role: &db::Role) -> Result<(), Error> {
        self.ensure_position(actor, role.position)?;
        self.ensure_admin_bits(actor, role.permissions)
    }
}

async fn compact(conn: &mut sqlx::SqliteConnection) -> Result<(), db::Error> {
    let listed = roles::list(conn).await?;
    for (index, role) in listed.iter().enumerate() {
        let position = i64::try_from(index).map_err(|_| db::Error::Corrupt("role position"))?;
        if role.position == position {
            continue;
        }
        roles::update(
            conn,
            role.id,
            RoleUpdate {
                name: role.name.clone(),
                color: role.color,
                permissions: role.permissions,
                position,
            },
        )
        .await?;
    }
    Ok(())
}

async fn audit(
    conn: &mut sqlx::SqliteConnection,
    actor: Id,
    action: &'static str,
    target_kind: &'static str,
    target_id: Option<Id>,
    details: &str,
) -> Result<(), Error> {
    audit_log::insert(
        conn,
        NewAuditEntry {
            id: Id::generate(),
            actor_id: Some(actor),
            action: action.to_owned(),
            target_kind: target_kind.to_owned(),
            target_id,
            details: details.to_owned(),
        },
    )
    .await?;
    Ok(())
}

fn role_name(name: &str) -> Result<&str, Error> {
    let len = name.chars().count();
    if (1..=100).contains(&len) {
        Ok(name)
    } else {
        Err(Error::Invalid("role name must be 1 to 100 characters"))
    }
}

fn bits(value: u64) -> Result<Permissions, Error> {
    permissions_from_bits(value).map_err(|_| Error::Invalid("unknown permission bits"))
}

fn required_id(bytes: impl AsRef<[u8]>) -> Result<Id, Error> {
    id_from_bytes(bytes.as_ref()).map_err(|_| Error::Invalid("id must be 16 bytes"))
}

fn unique_ids(raw: &[bytes::Bytes]) -> Result<Vec<Id>, Error> {
    let mut seen = HashSet::with_capacity(raw.len());
    let mut ids = Vec::with_capacity(raw.len());
    for value in raw {
        let id = required_id(value)?;
        if seen.insert(id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn wire_role(role: &db::Role) -> Result<v1::Role, Error> {
    Ok(v1::Role {
        id: id_to_bytes(role.id),
        name: role.name.clone(),
        color: role
            .color
            .map(|value| u32::try_from(value).map_err(|_| db::Error::Corrupt("role color")))
            .transpose()?,
        permissions: role.permissions.bits(),
        position: i32::try_from(role.position).map_err(|_| db::Error::Corrupt("role position"))?,
        is_default: role.is_default,
    })
}
