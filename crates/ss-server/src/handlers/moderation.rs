//! Kick, ban, move, and invites.
//!
//! A server mute is remembered until the process restarts; that flag lives
//! in the voice rooms. This module records the audit row for the other
//! moderator commands and closes the target's connections on kick and ban.

use std::sync::Arc;

use sqlx::Connection;
use ss_core::{Id, Permissions, Timestamp};
use ss_proto::{
    v1::{self, ack, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};

use crate::{
    auth,
    db::{self, NewAuditEntry, NewBan, NewInvite, audit_log, bans, invites, users},
    perms::PermissionService,
    registry::{self, ConnectionRegistry},
    voice::Voice,
};

use super::{CommandReply, Error};

const MAX_REASON: usize = 500;

/// Moderator commands. Mutations that change membership close connections.
pub struct Moderation {
    perms: Arc<PermissionService>,
    registry: Arc<ConnectionRegistry>,
    voice: Arc<Voice>,
}

impl Moderation {
    /// Shares the permission snapshot, connection table, and voice rooms.
    #[must_use]
    pub fn new(
        perms: Arc<PermissionService>,
        registry: Arc<ConnectionRegistry>,
        voice: Arc<Voice>,
    ) -> Self {
        Self {
            perms,
            registry,
            voice,
        }
    }

    /// Dispatches a moderator command. Other messages return `None`.
    pub async fn dispatch(
        &self,
        actor: Id,
        msg: &v1::client_envelope::Msg,
    ) -> Option<Result<CommandReply, Error>> {
        match msg {
            v1::client_envelope::Msg::CreateInvite(command) => {
                Some(self.create_invite(actor, command).await)
            }
            v1::client_envelope::Msg::RevokeInvite(command) => {
                Some(self.revoke_invite(actor, command).await)
            }
            v1::client_envelope::Msg::KickUser(command) => Some(self.kick(actor, command).await),
            v1::client_envelope::Msg::BanUser(command) => Some(self.ban(actor, command).await),
            v1::client_envelope::Msg::UnbanUser(command) => Some(self.unban(actor, command).await),
            v1::client_envelope::Msg::MoveUser(command) => {
                Some(self.move_user(actor, command).await)
            }
            _ => None,
        }
    }

    async fn create_invite(
        &self,
        actor: Id,
        command: &v1::CreateInvite,
    ) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::CREATE_INVITES, "missing create invites")?;
        let max_uses = use_cap(command.max_uses)?;
        let expires_at = future_expiry(command.expires_at, "invite expiry must be in the future")?;
        let code = auth::invite_code().map_err(|_| db::Error::Corrupt("random"))?;
        let invite = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let invite = invites::insert(
                &mut tx,
                NewInvite {
                    id: Id::generate(),
                    code: code.clone(),
                    created_by: actor,
                    max_uses,
                    expires_at,
                },
            )
            .await?;
            audit(
                &mut tx,
                actor,
                "invite.create",
                "invite",
                Some(invite.id),
                "{}",
            )
            .await?;
            tx.commit().await.map_err(db::Error::from)?;
            invite
        };
        let event = wire_invite(&invite, None);
        Ok(replied(
            ack::Result::Invite(wire_invite(&invite, Some(code))),
            server_envelope::Msg::InviteCreated(v1::InviteCreated {
                invite: Some(event),
            }),
        ))
    }

    async fn revoke_invite(
        &self,
        actor: Id,
        command: &v1::RevokeInvite,
    ) -> Result<CommandReply, Error> {
        let id = required_id(&command.invite_id, "invite id must be 16 bytes")?;
        let invite = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            let invite = invites::get(&mut tx, id)
                .await?
                .ok_or(Error::NotFound("unknown invite"))?;
            if invite.revoked_at.is_some() {
                return Err(Error::Invalid("invite is already revoked"));
            }
            if invite.created_by == actor {
                self.require(actor, Permissions::CREATE_INVITES, "missing create invites")?;
            } else {
                self.require(actor, Permissions::MANAGE_SERVER, "missing manage server")?;
            }
            let revoked_at = Timestamp::now();
            invites::revoke(&mut tx, id, revoked_at).await?;
            audit(&mut tx, actor, "invite.revoke", "invite", Some(id), "{}").await?;
            tx.commit().await.map_err(db::Error::from)?;
            db::Invite {
                revoked_at: Some(revoked_at),
                ..invite
            }
        };
        let wire = wire_invite(&invite, None);
        Ok(replied(
            ack::Result::Empty(v1::Empty {}),
            server_envelope::Msg::InviteRevoked(v1::InviteRevoked { invite: Some(wire) }),
        ))
    }

    async fn kick(&self, actor: Id, command: &v1::KickUser) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::KICK_MEMBERS, "missing kick members")?;
        let target = self.member(actor, &command.user_id).await?;
        let reason = reason_text(&command.reason)?;
        self.write_audit(actor, "user.kick", target, &reason_json(reason.as_deref()))
            .await?;
        self.registry.disconnect_user(
            target,
            &notice(server_envelope::Msg::Kicked(v1::Kicked {
                reason: reason.unwrap_or_default(),
                expires_at: None,
            })),
            registry::CLOSE_KICKED,
            b"kicked",
        );
        Ok(empty())
    }

    async fn ban(&self, actor: Id, command: &v1::BanUser) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::BAN_MEMBERS, "missing ban members")?;
        let target = self.member(actor, &command.user_id).await?;
        let reason = reason_text(&command.reason)?;
        let expires_at = future_expiry(command.expires_at, "ban expiry must be in the future")?;
        if active_ban(self.perms.as_ref(), target).await? {
            return Err(Error::Invalid("already banned"));
        }
        self.insert_ban(actor, target, reason.clone(), expires_at)
            .await?;
        self.registry.disconnect_user(
            target,
            &notice(server_envelope::Msg::Banned(v1::Banned {
                reason: reason.unwrap_or_default(),
                expires_at: expires_at.map(Timestamp::as_millis),
            })),
            registry::CLOSE_BANNED,
            b"banned",
        );
        Ok(empty())
    }

    async fn unban(&self, actor: Id, command: &v1::UnbanUser) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::BAN_MEMBERS, "missing ban members")?;
        let target = self.member(actor, &command.user_id).await?;
        let mut guard = self.perms.pool().write().await;
        let mut tx = guard.begin().await.map_err(db::Error::from)?;
        let lifted = bans::lift_user(&mut tx, target, Timestamp::now()).await?;
        if lifted == 0 {
            return Err(Error::NotFound("not banned"));
        }
        audit(&mut tx, actor, "user.unban", "user", Some(target), "{}").await?;
        tx.commit().await.map_err(db::Error::from)?;
        Ok(empty())
    }

    async fn move_user(&self, actor: Id, command: &v1::MoveUser) -> Result<CommandReply, Error> {
        self.require(actor, Permissions::MOVE_MEMBERS, "missing move members")?;
        let target = self.member(actor, &command.user_id).await?;
        let channel = required_id(&command.channel_id, "channel id must be 16 bytes")?;
        let (outcome, conn) = self.voice.move_member(target, channel).await?;
        self.write_audit(
            actor,
            "user.move",
            target,
            &serde_json::json!({ "channel": channel.to_string() }).to_string(),
        )
        .await?;
        for event in outcome.direct {
            self.registry.enqueue(
                conn,
                v1::ServerEnvelope {
                    request_id: 0,
                    msg: Some(event),
                },
            );
        }
        Ok(CommandReply {
            ack: outcome.ack,
            events: outcome.broadcast,
        })
    }

    async fn insert_ban(
        &self,
        actor: Id,
        target: Id,
        reason: Option<String>,
        expires_at: Option<Timestamp>,
    ) -> Result<(), Error> {
        let mut guard = self.perms.pool().write().await;
        let mut tx = guard.begin().await.map_err(db::Error::from)?;
        bans::insert(
            &mut tx,
            NewBan {
                id: Id::generate(),
                user_id: target,
                banned_by: actor,
                reason: reason.clone(),
                expires_at,
            },
        )
        .await?;
        audit(
            &mut tx,
            actor,
            "user.ban",
            "user",
            Some(target),
            &reason_json(reason.as_deref()),
        )
        .await?;
        tx.commit().await.map_err(db::Error::from)?;
        Ok(())
    }

    async fn member(&self, actor: Id, raw: &[u8]) -> Result<Id, Error> {
        let target = required_id(raw, "user id must be 16 bytes")?;
        if actor == target {
            return Err(Error::Forbidden("cannot moderate yourself"));
        }
        if !self.perms.can_act_on(actor, target) {
            return Err(Error::Forbidden("cannot act on that member"));
        }
        let mut conn = self.perms.pool().read().await?;
        if users::get(&mut conn, target).await?.is_none() {
            return Err(Error::NotFound("unknown user"));
        }
        Ok(target)
    }

    fn require(
        &self,
        actor: Id,
        permission: Permissions,
        message: &'static str,
    ) -> Result<(), Error> {
        self.perms
            .check(actor, None, permission)
            .map_err(|_| Error::Forbidden(message))
    }

    async fn write_audit(
        &self,
        actor: Id,
        action: &str,
        target: Id,
        details: &str,
    ) -> Result<(), Error> {
        let mut guard = self.perms.pool().write().await;
        audit(&mut guard, actor, action, "user", Some(target), details).await
    }
}

/// Records a permanent ban. A live connection stays up until the process
/// restarts or a moderator kicks it; the next `Hello` is rejected.
pub async fn ban_account(
    pool: &db::Pool,
    username: &str,
    reason: Option<&str>,
) -> Result<(), Error> {
    let reason = reason.map(reason_text).transpose()?.flatten();
    let mut guard = pool.write().await;
    let mut tx = guard.begin().await.map_err(db::Error::from)?;
    let user = users::get_by_username(&mut tx, username)
        .await?
        .ok_or(Error::NotFound("unknown user"))?;
    bans::insert(
        &mut tx,
        NewBan {
            id: Id::generate(),
            user_id: user.id,
            banned_by: user.id,
            reason: reason.clone(),
            expires_at: None,
        },
    )
    .await?;
    system_audit(
        &mut tx,
        "user.ban",
        Some(user.id),
        &reason_json(reason.as_deref()),
    )
    .await?;
    tx.commit().await.map_err(db::Error::from)?;
    Ok(())
}

/// Lifts every open ban for `username`.
pub async fn unban_account(pool: &db::Pool, username: &str) -> Result<(), Error> {
    let mut guard = pool.write().await;
    let mut tx = guard.begin().await.map_err(db::Error::from)?;
    let user = users::get_by_username(&mut tx, username)
        .await?
        .ok_or(Error::NotFound("unknown user"))?;
    let lifted = bans::lift_user(&mut tx, user.id, Timestamp::now()).await?;
    if lifted == 0 {
        return Err(Error::NotFound("not banned"));
    }
    system_audit(&mut tx, "user.unban", Some(user.id), "{}").await?;
    tx.commit().await.map_err(db::Error::from)?;
    Ok(())
}

/// Grants the seeded Admin role. The running process keeps its permission
/// snapshot until it restarts.
pub async fn grant_admin(pool: &db::Pool, username: &str) -> Result<bool, Error> {
    let mut guard = pool.write().await;
    let mut tx = guard.begin().await.map_err(db::Error::from)?;
    let user = users::get_by_username(&mut tx, username)
        .await?
        .ok_or(Error::NotFound("unknown user"))?;
    let held = db::roles::roles_for_user(&mut tx, user.id).await?;
    if held.iter().any(|role| role.id == db::ADMIN_ROLE_ID) {
        return Ok(false);
    }
    db::roles::assign(&mut tx, user.id, db::ADMIN_ROLE_ID).await?;
    system_audit(&mut tx, "role.grant-admin", Some(user.id), "{}").await?;
    tx.commit().await.map_err(db::Error::from)?;
    Ok(true)
}

async fn active_ban(perms: &PermissionService, user: Id) -> Result<bool, Error> {
    let mut conn = perms.pool().read().await?;
    let now = Timestamp::now();
    let bans = bans::list_for_user(&mut conn, user).await?;
    Ok(bans
        .iter()
        .any(|ban| ban.lifted_at.is_none() && ban.expires_at.is_none_or(|expires| expires > now)))
}

fn reason_text(text: &str) -> Result<Option<String>, Error> {
    if text.chars().count() > MAX_REASON {
        return Err(Error::Invalid("reason must be at most 500 characters"));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_owned()))
    }
}

fn reason_json(reason: Option<&str>) -> String {
    serde_json::json!({ "reason": reason.unwrap_or("") }).to_string()
}

fn use_cap(max_uses: Option<u32>) -> Result<Option<i64>, Error> {
    match max_uses {
        Some(0) => Err(Error::Invalid("max uses must be at least 1")),
        Some(value) => Ok(Some(i64::from(value))),
        None => Ok(None),
    }
}

fn future_expiry(raw: Option<i64>, message: &'static str) -> Result<Option<Timestamp>, Error> {
    let Some(millis) = raw else {
        return Ok(None);
    };
    if millis <= Timestamp::now().as_millis() {
        return Err(Error::Invalid(message));
    }
    Ok(Some(Timestamp::from_millis(millis)))
}

fn required_id(bytes: &[u8], message: &'static str) -> Result<Id, Error> {
    id_from_bytes(bytes).map_err(|_| Error::Invalid(message))
}

fn notice(msg: server_envelope::Msg) -> v1::ServerEnvelope {
    v1::ServerEnvelope {
        request_id: 0,
        msg: Some(msg),
    }
}

fn empty() -> CommandReply {
    CommandReply {
        ack: ack::Result::Empty(v1::Empty {}),
        events: Vec::new(),
    }
}

fn replied(ack: ack::Result, event: server_envelope::Msg) -> CommandReply {
    CommandReply {
        ack,
        events: vec![event],
    }
}

fn wire_invite(invite: &db::Invite, code: Option<String>) -> v1::Invite {
    v1::Invite {
        id: id_to_bytes(invite.id),
        code: code.unwrap_or_default(),
        created_by: id_to_bytes(invite.created_by),
        max_uses: invite.max_uses.and_then(|value| u32::try_from(value).ok()),
        uses: u32::try_from(invite.uses).unwrap_or(u32::MAX),
        expires_at: invite.expires_at.map(Timestamp::as_millis),
        created_at: invite.created_at.as_millis(),
        revoked_at: invite.revoked_at.map(Timestamp::as_millis),
    }
}

async fn system_audit(
    conn: &mut sqlx::SqliteConnection,
    action: &str,
    target_id: Option<Id>,
    details: &str,
) -> Result<(), Error> {
    audit_log::insert(
        conn,
        NewAuditEntry {
            id: Id::generate(),
            actor_id: None,
            action: action.to_owned(),
            target_kind: "user".to_owned(),
            target_id,
            details: details.to_owned(),
        },
    )
    .await?;
    Ok(())
}

async fn audit(
    conn: &mut sqlx::SqliteConnection,
    actor: Id,
    action: &str,
    target_kind: &str,
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
