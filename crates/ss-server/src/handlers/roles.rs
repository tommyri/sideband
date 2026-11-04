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

/// Ack body plus the events to send after it.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandReply {
    /// Payload for [`ss_proto::v1::ServerEnvelope::ack`].
    pub ack: ack::Result,
    /// Event frames. The connection sends each with request id 0.
    pub events: Vec<server_envelope::Msg>,
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
    /// The command arrived faster than its quota. The duration is how long
    /// until the next one would be accepted.
    #[error("{0}")]
    RateLimited(&'static str, std::time::Duration),
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
            Self::RateLimited(..) => ErrorCode::RateLimited,
            Self::Database(_) => ErrorCode::Internal,
        }
    }

    /// How long a rate-limited caller should wait. `None` for other failures.
    pub(crate) fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Self::RateLimited(_, wait) => Some(*wait),
            _ => None,
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
            v1::client_envelope::Msg::MoveRole(command) => {
                Some(self.move_role(actor, command).await)
            }
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
        let name = role_name(&command.name)?;
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
            events: vec![server_envelope::Msg::RoleCreated(v1::RoleCreated {
                role: Some(wire),
            })],
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
                Some(name) => role_name(name)?,
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
                    color: if command.clear_color {
                        None
                    } else {
                        command
                            .color
                            .map_or(current.color, |color| Some(i64::from(color)))
                    },
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
            events: vec![server_envelope::Msg::RoleUpdated(v1::RoleUpdated {
                role: Some(wire),
            })],
        })
    }

    /// Atomically moves a role in highest-first order, preserving all other ordering.
    pub async fn move_role(
        &self,
        actor: Id,
        command: &v1::MoveRole,
    ) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::MANAGE_ROLES)?;
        let id = required_id(&command.role_id)?;
        let before = command
            .before_role_id
            .as_ref()
            .map(required_id)
            .transpose()?;
        let mut guard = self.perms.pool().write().await;
        let mut tx = guard.begin().await.map_err(db::Error::from)?;
        let mut listed = roles::list(&mut tx).await?;
        // Normalization must not split equal ranks outside this actor's authority.
        if !self.perms.is_administrator(actor)
            && listed.windows(2).any(|pair| {
                pair[0].position == pair[1].position
                    && pair[0].position >= self.perms.highest_role_position(actor)
            })
        {
            return Err(Error::Forbidden(
                "an administrator must resolve equal protected ranks first",
            ));
        }
        let index = listed
            .iter()
            .position(|r| r.id == id)
            .ok_or(Error::NotFound("role not found"))?;
        self.ensure_can_edit(actor, &listed[index])?;
        if before == Some(id) {
            return Err(Error::Invalid("cannot move before itself"));
        }
        if let Some(target) = before {
            let target = listed
                .iter()
                .find(|r| r.id == target)
                .ok_or(Error::NotFound("role not found"))?;
            self.ensure_can_edit(actor, target)?;
        }
        let role = listed.remove(index);
        let destination = before.map_or(Ok(0), |target| {
            listed
                .iter()
                .position(|r| r.id == target)
                .map(|i| i + 1)
                .ok_or(Error::NotFound("role not found"))
        })?;
        listed.insert(destination, role);
        let events = reorder(&mut tx, &listed).await?;
        audit(&mut tx, actor, "role.move", "role", Some(id), "{}").await?;
        tx.commit().await.map_err(db::Error::from)?;
        self.perms.reload().await?;
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            events,
        })
    }

    /// Deletes a role, clears its grants and overrides, then compacts positions.
    pub async fn delete(&self, actor: Id, command: &v1::DeleteRole) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::MANAGE_ROLES)?;
        let id = required_id(&command.role_id)?;
        let (role, mut events) = {
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
            let events = compact(&mut tx).await?;
            audit(&mut tx, actor, "role.delete", "role", Some(id), "{}").await?;
            tx.commit().await.map_err(db::Error::from)?;
            self.perms.reload().await?;
            (role, events)
        };
        tracing::info!(%actor, role_id = %role.id, "role deleted");
        events.insert(
            0,
            server_envelope::Msg::RoleDeleted(v1::RoleDeleted {
                role: Some(wire_role(&role)?),
            }),
        );
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            events,
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
            events: vec![server_envelope::Msg::MemberRolesUpdated(
                v1::MemberRolesUpdated {
                    user_id: id_to_bytes(target),
                    role_ids: stored.iter().copied().map(id_to_bytes).collect(),
                },
            )],
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
        let mut conn = self.perms.pool().read().await?;
        let channel = channels::get(&mut conn, channel_id)
            .await?
            .ok_or(Error::NotFound("channel not found"))?;
        let wire = crate::snapshot::wire_channel(&channel, &[], None)?;
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty::default()),
            events: vec![ss_proto::v1::server_envelope::Msg::ChannelUpdated(
                v1::ChannelUpdated {
                    channel: Some(wire),
                },
            )],
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

async fn compact(conn: &mut sqlx::SqliteConnection) -> Result<Vec<server_envelope::Msg>, Error> {
    let listed = roles::list(conn).await?;
    reorder(conn, &listed).await
}
async fn reorder(
    conn: &mut sqlx::SqliteConnection,
    listed: &[db::Role],
) -> Result<Vec<server_envelope::Msg>, Error> {
    let mut events = Vec::new();
    for (index, role) in listed.iter().enumerate() {
        let position = i32::try_from(index).map_err(|_| Error::Invalid("too many roles"))?;
        if role.position == i64::from(position) {
            continue;
        }
        let updated = roles::update(
            conn,
            role.id,
            RoleUpdate {
                name: role.name.clone(),
                color: role.color,
                permissions: role.permissions,
                position: i64::from(position),
            },
        )
        .await?;
        events.push(server_envelope::Msg::RoleUpdated(v1::RoleUpdated {
            role: Some(wire_role(&updated)?),
        }));
    }
    Ok(events)
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

fn role_name(name: &str) -> Result<String, Error> {
    crate::text::name(name, 100).map_err(|error| match error {
        crate::text::NameError::Length => Error::Invalid("role name must be 1 to 100 characters"),
        crate::text::NameError::Hidden => Error::Invalid("role name contains a hidden character"),
    })
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
