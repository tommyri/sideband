//! Incoming webhooks.
//!
//! `CreateWebhook` returns the token once. Posts use the HTTP route, not a
//! control command. The stored message has no author; clients show
//! `webhook_name` with a bot tag.

use std::{num::NonZeroU32, sync::Arc};

use governor::{Quota, RateLimiter, clock::DefaultClock, state::keyed::DefaultKeyedStateStore};
use serde_json::{Map, Value};
use sqlx::Connection;
use ss_core::{ChannelKind, Id, Permissions};
use ss_proto::{
    v1::{self, ack, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};

use crate::{
    auth,
    db::{self, NewAuditEntry, NewMessage, NewWebhook, audit_log, channels, messages, webhooks},
    events::EventBus,
    perms::PermissionService,
    previews::{PreviewJob, PreviewQueue},
};

use super::{CommandReply, Error, messages::to_proto};

const MAX_NAME: usize = 80;
const MAX_CONTENT: usize = 2000;
const POSTS_PER_MINUTE: u32 = 30;

type WebhookLimiter = RateLimiter<Id, DefaultKeyedStateStore<Id>, DefaultClock>;

/// Create, revoke, and execute incoming webhooks.
pub struct Webhooks {
    perms: Arc<PermissionService>,
    posts: WebhookLimiter,
    bus: EventBus,
    previews: PreviewQueue,
}

impl Webhooks {
    /// Thirty posts per minute for each webhook.
    #[must_use]
    pub fn new(perms: Arc<PermissionService>, previews: PreviewQueue, bus: EventBus) -> Self {
        let quota = Quota::per_minute(NonZeroU32::new(POSTS_PER_MINUTE).expect("thirty"));
        Self {
            perms,
            posts: RateLimiter::keyed(quota),
            bus,
            previews,
        }
    }

    /// Dispatches create and delete. Other messages return `None`.
    pub async fn dispatch(
        &self,
        actor: Id,
        msg: &v1::client_envelope::Msg,
    ) -> Option<Result<CommandReply, Error>> {
        match msg {
            v1::client_envelope::Msg::CreateWebhook(command) => {
                Some(self.create(actor, command).await)
            }
            v1::client_envelope::Msg::DeleteWebhook(command) => {
                Some(self.delete(actor, command).await)
            }
            _ => None,
        }
    }

    /// Posts `body` when `token` matches the webhook. A bad or revoked token
    /// is not found.
    pub async fn execute(&self, id: Id, token: &str, body: &Value) -> Result<db::Message, Error> {
        let map = body
            .as_object()
            .ok_or(Error::Invalid("body must be a json object"))?;
        let content = flatten(map)?;
        let webhook = self.open_webhook(id, token).await?;
        let name = display_name(map, &webhook.name)?;
        if self.posts.check_key(&webhook.id).is_err() {
            return Err(Error::RateLimited("webhook is posting too quickly"));
        }
        let mut conn = self.perms.pool().write().await;
        require_text(&mut conn, webhook.channel_id).await?;
        let stored = messages::insert(
            &mut conn,
            NewMessage {
                id: Id::generate(),
                channel_id: webhook.channel_id,
                author_id: None,
                webhook_id: Some(webhook.id),
                webhook_name: Some(name),
                content: content.clone(),
                reply_to: None,
                mentions: Vec::new(),
                mentions_everyone: false,
            },
        )
        .await?;
        let wire = to_proto(&mut conn, &stored).await?;
        drop(conn);
        self.bus.publish(
            None,
            server_envelope::Msg::MessageCreated(v1::MessageCreated {
                message: Some(wire),
                nonce: None,
            }),
        );
        self.previews.enqueue(PreviewJob {
            message_id: stored.id,
            content,
        });
        Ok(stored)
    }

    async fn create(&self, actor: Id, command: &v1::CreateWebhook) -> Result<CommandReply, Error> {
        self.require(actor)?;
        let channel_id = required_id(&command.channel_id, "channel id must be 16 bytes")?;
        let name = hook_name(&command.name)?;
        let token = auth::invite_code().map_err(|_| db::Error::Corrupt("random"))?;
        let webhook = {
            let mut guard = self.perms.pool().write().await;
            let mut tx = guard.begin().await.map_err(db::Error::from)?;
            require_text(&mut tx, channel_id).await?;
            let webhook = webhooks::insert(
                &mut tx,
                NewWebhook {
                    id: Id::generate(),
                    channel_id,
                    name: name.clone(),
                    token_hash: auth::hash_token(&token),
                    created_by: actor,
                },
            )
            .await?;
            audit(&mut tx, actor, "webhook.create", webhook.id, &channel_id).await?;
            tx.commit().await.map_err(db::Error::from)?;
            webhook
        };
        Ok(CommandReply {
            ack: ack::Result::Webhook(wire_webhook(&webhook, Some(token))),
            events: Vec::new(),
        })
    }

    async fn delete(&self, actor: Id, command: &v1::DeleteWebhook) -> Result<CommandReply, Error> {
        self.require(actor)?;
        let id = required_id(&command.webhook_id, "webhook id must be 16 bytes")?;
        let mut guard = self.perms.pool().write().await;
        let mut tx = guard.begin().await.map_err(db::Error::from)?;
        let webhook = webhooks::get(&mut tx, id)
            .await?
            .filter(|webhook| webhook.revoked_at.is_none())
            .ok_or(Error::NotFound("unknown webhook"))?;
        webhooks::revoke(&mut tx, id, ss_core::Timestamp::now()).await?;
        audit(&mut tx, actor, "webhook.delete", id, &webhook.channel_id).await?;
        tx.commit().await.map_err(db::Error::from)?;
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty {}),
            events: Vec::new(),
        })
    }

    async fn open_webhook(&self, id: Id, token: &str) -> Result<db::Webhook, Error> {
        let mut conn = self.perms.pool().read().await?;
        let webhook = webhooks::get(&mut conn, id)
            .await?
            .filter(|webhook| webhook.revoked_at.is_none())
            .ok_or(Error::NotFound("unknown webhook"))?;
        if webhook.token_hash != auth::hash_token(token) {
            return Err(Error::NotFound("unknown webhook"));
        }
        Ok(webhook)
    }

    fn require(&self, actor: Id) -> Result<(), Error> {
        self.perms
            .check(actor, None, Permissions::MANAGE_SERVER)
            .map_err(|_| Error::Forbidden("missing manage server"))
    }
}

fn flatten(map: &Map<String, Value>) -> Result<String, Error> {
    let mut parts = Vec::new();
    if let Some(content) = text_of(map.get("content")) {
        parts.push(content);
    }
    if let Some(embeds) = map.get("embeds").and_then(Value::as_array) {
        for embed in embeds.iter().take(3) {
            if let Some(text) = embed_text(embed) {
                parts.push(text);
            }
        }
    }
    if parts.is_empty()
        && let Some(text) = github_text(map).or_else(|| grafana_text(map))
    {
        parts.push(text);
    }
    let body = parts.join("\n");
    match body.chars().count() {
        0 => Err(Error::Invalid("content is empty")),
        count if count > MAX_CONTENT => {
            Err(Error::Invalid("content must be at most 2000 characters"))
        }
        _ => Ok(body),
    }
}

fn embed_text(embed: &Value) -> Option<String> {
    let title = text_of(embed.get("title"));
    let description = text_of(embed.get("description"));
    match (title, description) {
        (Some(title), Some(description)) => Some(format!("{title}\n{description}")),
        (Some(title), None) => Some(title),
        (None, Some(description)) => Some(description),
        (None, None) => None,
    }
}

fn github_text(map: &Map<String, Value>) -> Option<String> {
    let repo = map
        .get("repository")?
        .get("full_name")
        .and_then(Value::as_str)?;
    let who = map
        .get("pusher")
        .and_then(|pusher| pusher.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("someone");
    let reference = map.get("ref").and_then(Value::as_str).unwrap_or("");
    let commit = map
        .get("commits")
        .and_then(Value::as_array)
        .and_then(|commits| commits.first())
        .and_then(|commit| text_of(commit.get("message")));
    let head = if reference.is_empty() {
        format!("{who} pushed to {repo}")
    } else {
        format!("{who} pushed to {repo} ({reference})")
    };
    Some(match commit {
        Some(message) => format!("{head}\n{message}"),
        None => head,
    })
}

fn grafana_text(map: &Map<String, Value>) -> Option<String> {
    let title = text_of(map.get("title"))?;
    let message = text_of(map.get("message")).unwrap_or_default();
    let state = text_of(map.get("state")).unwrap_or_default();
    let head = if state.is_empty() {
        title
    } else {
        format!("{state}: {title}")
    };
    if message.is_empty() {
        Some(head)
    } else {
        Some(format!("{head}\n{message}"))
    }
}

fn text_of(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn display_name(map: &Map<String, Value>, fallback: &str) -> Result<String, Error> {
    let Some(name) = text_of(map.get("username")) else {
        return Ok(fallback.to_owned());
    };
    if name.chars().count() > MAX_NAME {
        return Err(Error::Invalid("username must be at most 80 characters"));
    }
    Ok(name)
}

fn hook_name(name: &str) -> Result<String, Error> {
    let name = name.trim();
    if (1..=MAX_NAME).contains(&name.chars().count()) {
        Ok(name.to_owned())
    } else {
        Err(Error::Invalid("webhook name must be 1 to 80 characters"))
    }
}

async fn require_text(conn: &mut sqlx::SqliteConnection, id: Id) -> Result<(), Error> {
    let channel = channels::get(conn, id)
        .await?
        .filter(|channel| channel.deleted_at.is_none())
        .ok_or(Error::NotFound("unknown channel"))?;
    if channel.kind == ChannelKind::Text {
        Ok(())
    } else {
        Err(Error::Invalid("not a text channel"))
    }
}

fn required_id(bytes: &[u8], message: &'static str) -> Result<Id, Error> {
    id_from_bytes(bytes).map_err(|_| Error::Invalid(message))
}

fn wire_webhook(webhook: &db::Webhook, token: Option<String>) -> v1::Webhook {
    v1::Webhook {
        id: id_to_bytes(webhook.id),
        channel_id: id_to_bytes(webhook.channel_id),
        name: webhook.name.clone(),
        token,
        created_by: id_to_bytes(webhook.created_by),
        created_at: webhook.created_at.as_millis(),
    }
}

async fn audit(
    conn: &mut sqlx::SqliteConnection,
    actor: Id,
    action: &str,
    webhook: Id,
    channel: &Id,
) -> Result<(), Error> {
    let details = serde_json::json!({ "channel": channel.to_string() }).to_string();
    audit_log::insert(
        conn,
        NewAuditEntry {
            id: Id::generate(),
            actor_id: Some(actor),
            action: action.to_owned(),
            target_kind: "webhook".to_owned(),
            target_id: Some(webhook),
            details,
        },
    )
    .await?;
    Ok(())
}
