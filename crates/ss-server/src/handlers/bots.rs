//! Bot accounts and their tokens (T-080).
//!
//! A bot is a `users` row with kind 1. `CreateBot`, `ResetBotToken`,
//! `RevokeBotToken` and `ListBots` need `MANAGE_SERVER`; a bot cannot run
//! them. A token is returned once, on the ack, and only its SHA-256 is stored
//! (the webhook pattern). Its scopes may only hold bits the issuer holds, and
//! [`crate::perms`] caps the bot by them on every check.
//!
//! Revoking or replacing a token closes the QUIC connections that used it
//! (`SessionRevoked`, code 6) and ends its HTTPS event streams at once.
//! [`Bots::spend`] is the per-bot request budget, shared by both transports.

use std::{collections::HashMap, sync::Arc, time::Duration};

use parking_lot::Mutex;
use sqlx::Connection;
use ss_core::{Id, Permissions, Timestamp, UserKind};
use ss_proto::{
    v1::{self, ack, client_envelope::Msg, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};
use tokio::sync::watch;
use zeroize::Zeroizing;

use super::{CommandReply, Error};
use crate::{
    auth,
    db::{self, NewAuditEntry, NewBotToken, NewUser, audit_log, bots, users},
    limits::{self, KeyedLimiter, quota},
    perms::PermissionService,
};

/// Stored in `users.password_hash` for a bot. Not a PHC string, so no
/// password verifies against it; login also skips bot accounts.
const NO_PASSWORD: &str = "!bot";
const MAX_DISPLAY: usize = 64;

/// Bot management commands, the per-bot budget, and live event streams.
pub struct Bots {
    perms: Arc<PermissionService>,
    sessions: Arc<auth::Sessions>,
    requests: KeyedLimiter<Id>,
    streams: Arc<Mutex<HashMap<Id, Streams>>>,
}

/// Event streams open with one token.
struct Streams {
    revoked: watch::Sender<bool>,
    open: usize,
}

/// One open event stream. Dropping it frees the slot.
pub(crate) struct StreamSlot {
    streams: Arc<Mutex<HashMap<Id, Streams>>>,
    token: Id,
    /// Becomes `true` when the token is revoked or replaced.
    pub revoked: watch::Receiver<bool>,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut streams = self.streams.lock();
        if let Some(entry) = streams.get_mut(&self.token) {
            entry.open = entry.open.saturating_sub(1);
            if entry.open == 0 {
                streams.remove(&self.token);
            }
        }
    }
}

impl Bots {
    /// Shares `perms` and `sessions` with the connection owner.
    #[must_use]
    pub(crate) fn new(perms: Arc<PermissionService>, sessions: Arc<auth::Sessions>) -> Self {
        Self {
            perms,
            sessions,
            requests: KeyedLimiter::new(quota(
                limits::BOT_REQUEST_BURST,
                limits::BOT_REQUEST_PERIOD,
            )),
            streams: Arc::default(),
        }
    }

    /// Spends one request of `user`'s per-bot budget. Members always pass.
    pub(crate) fn spend(&self, user: Id) -> Result<(), Duration> {
        if self.perms.is_bot(user) {
            self.requests.check(&user)
        } else {
            Ok(())
        }
    }

    /// Opens an event stream for `token`, at most
    /// [`limits::BOT_STREAMS`] at once per token.
    pub(crate) fn open_stream(&self, token: Id) -> Option<StreamSlot> {
        let mut streams = self.streams.lock();
        let entry = streams.entry(token).or_insert_with(|| Streams {
            revoked: watch::channel(false).0,
            open: 0,
        });
        if entry.open >= limits::BOT_STREAMS {
            return None;
        }
        entry.open += 1;
        Some(StreamSlot {
            streams: Arc::clone(&self.streams),
            token,
            revoked: entry.revoked.subscribe(),
        })
    }

    /// Closes everything that used `token`: QUIC connections and streams.
    pub(crate) fn end(&self, token: Id) {
        self.sessions.end(token);
        if let Some(entry) = self.streams.lock().get(&token) {
            let _ = entry.revoked.send(true);
        }
    }

    /// Dispatches the bot commands. Other messages return `None`.
    pub(crate) async fn dispatch(
        &self,
        actor: Id,
        msg: &Msg,
    ) -> Option<Result<CommandReply, Error>> {
        match msg {
            Msg::CreateBot(command) => Some(self.create(actor, command).await),
            Msg::ResetBotToken(command) => Some(self.reset(actor, command).await),
            Msg::RevokeBotToken(command) => Some(self.revoke(actor, command).await),
            Msg::ListBots(_) => Some(self.list(actor).await),
            _ => None,
        }
    }

    async fn create(&self, actor: Id, command: &v1::CreateBot) -> Result<CommandReply, Error> {
        self.require(actor)?;
        let scopes = self.scopes(actor, command.scopes)?;
        let (user, row, token) = create_account(
            self.perms.pool(),
            Some(actor),
            &command.username,
            &command.display_name,
            scopes,
        )
        .await?;
        self.perms.reload().await?;
        let member = {
            let mut conn = self.perms.pool().read().await?;
            super::member(&mut conn, &user, crate::snapshot::offline_presence(user.id)).await?
        };
        Ok(CommandReply {
            ack: ack::Result::Bot(wire_bot(&user, Some(&row), Some(token.to_string()))),
            events: vec![server_envelope::Msg::MemberJoined(v1::MemberJoined {
                member: Some(member),
            })],
        })
    }

    async fn reset(&self, actor: Id, command: &v1::ResetBotToken) -> Result<CommandReply, Error> {
        self.require(actor)?;
        let scopes = self.scopes(actor, command.scopes)?;
        let bot = self.target(actor, &command.bot_id).await?;
        let (old, row, token) =
            replace_token(self.perms.pool(), Some(actor), bot.id, scopes).await?;
        self.perms.reload().await?;
        if let Some(old) = old {
            self.end(old);
        }
        Ok(CommandReply {
            ack: ack::Result::Bot(wire_bot(&bot, Some(&row), Some(token.to_string()))),
            events: Vec::new(),
        })
    }

    async fn revoke(&self, actor: Id, command: &v1::RevokeBotToken) -> Result<CommandReply, Error> {
        self.require(actor)?;
        let bot = self.target(actor, &command.bot_id).await?;
        let old = revoke_token(self.perms.pool(), Some(actor), bot.id).await?;
        self.perms.reload().await?;
        self.end(old);
        Ok(CommandReply {
            ack: ack::Result::Empty(v1::Empty {}),
            events: Vec::new(),
        })
    }

    async fn list(&self, actor: Id) -> Result<CommandReply, Error> {
        self.require(actor)?;
        let bots = list_accounts(self.perms.pool())
            .await?
            .iter()
            .map(|(user, token)| wire_bot(user, token.as_ref(), None))
            .collect();
        Ok(CommandReply {
            ack: ack::Result::Bots(v1::BotList { bots }),
            events: Vec::new(),
        })
    }

    /// `MANAGE_SERVER`, held by a member: bots never manage bots, so a token
    /// cannot mint another with more reach.
    fn require(&self, actor: Id) -> Result<(), Error> {
        if self.perms.is_bot(actor) {
            return Err(Error::Forbidden("bots cannot manage bots"));
        }
        self.perms
            .check(actor, None, Permissions::MANAGE_SERVER)
            .map_err(|_| Error::Forbidden("missing manage server"))
    }

    /// Known bits only, and only bits the issuer holds on the server.
    fn scopes(&self, actor: Id, bits: u64) -> Result<Permissions, Error> {
        let scopes =
            Permissions::from_bits(bits).ok_or(Error::Invalid("scopes contain unknown bits"))?;
        if !self.perms.holds(actor, None, scopes) {
            return Err(Error::Forbidden("cannot grant a scope you do not hold"));
        }
        Ok(scopes)
    }

    /// The bot named by `bytes`, when `actor` outranks it.
    async fn target(&self, actor: Id, bytes: &[u8]) -> Result<db::User, Error> {
        let id = id_from_bytes(bytes).map_err(|_| Error::Invalid("bot id must be 16 bytes"))?;
        let bot = {
            let mut conn = self.perms.pool().read().await?;
            users::get(&mut conn, id).await?
        }
        .filter(|user| user.kind == UserKind::Bot)
        .ok_or(Error::NotFound("unknown bot"))?;
        if !self.perms.can_act_on(actor, bot.id) {
            return Err(Error::Forbidden("the bot's role is not below yours"));
        }
        Ok(bot)
    }
}

/// Creates bot account `username` with a token of `scopes`. `issuer` is the
/// member who asked, `None` for the operator CLI. Callers check permission;
/// the name follows a member's rules, look-alike check included.
pub async fn create_account(
    pool: &db::Pool,
    issuer: Option<Id>,
    username: &str,
    display_name: &str,
    scopes: Permissions,
) -> Result<(db::User, db::BotToken, Zeroizing<String>), Error> {
    let display_name = if display_name.trim().is_empty() {
        username.to_owned()
    } else {
        display(display_name)?
    };
    let (token, hash) = auth::bot_token().map_err(|_| db::Error::Corrupt("random"))?;
    let mut guard = pool.write().await;
    let mut tx = guard.begin().await.map_err(db::Error::from)?;
    auth::check_bot_username(&mut tx, username)
        .await
        .map_err(|error| match error {
            auth::Error::Conflict => Error::Invalid("username is not available"),
            auth::Error::Invalid(message) => Error::Invalid(message),
            _ => Error::Database(db::Error::Corrupt("username check")),
        })?;
    let user = users::insert(
        &mut tx,
        NewUser {
            id: Id::generate(),
            username: username.to_owned(),
            display_name,
            password_hash: NO_PASSWORD.to_owned(),
            kind: UserKind::Bot,
        },
    )
    .await
    .map_err(|error| match error {
        db::Error::Conflict => Error::Invalid("username is not available"),
        other => other.into(),
    })?;
    let row = issue(&mut tx, user.id, hash, scopes, issuer).await?;
    audit(&mut tx, issuer, "bot.create", user.id, scopes).await?;
    tx.commit().await.map_err(db::Error::from)?;
    Ok((user, row, token))
}

/// Revokes `bot`'s live token, if any, and issues one of `scopes`. Returns
/// the revoked token's id. A running server closes what used it at once when
/// it made the change, else on its next sweep (at most 30 s).
pub async fn replace_token(
    pool: &db::Pool,
    issuer: Option<Id>,
    bot: Id,
    scopes: Permissions,
) -> Result<(Option<Id>, db::BotToken, Zeroizing<String>), Error> {
    let (token, hash) = auth::bot_token().map_err(|_| db::Error::Corrupt("random"))?;
    let mut guard = pool.write().await;
    let mut tx = guard.begin().await.map_err(db::Error::from)?;
    let old = bots::revoke_for(&mut tx, bot, Timestamp::now()).await?;
    let row = issue(&mut tx, bot, hash, scopes, issuer).await?;
    audit(&mut tx, issuer, "bot.token.reset", bot, scopes).await?;
    tx.commit().await.map_err(db::Error::from)?;
    Ok((old, row, token))
}

/// Revokes `bot`'s live token and returns its id.
pub async fn revoke_token(pool: &db::Pool, issuer: Option<Id>, bot: Id) -> Result<Id, Error> {
    let mut guard = pool.write().await;
    let mut tx = guard.begin().await.map_err(db::Error::from)?;
    let old = bots::revoke_for(&mut tx, bot, Timestamp::now())
        .await?
        .ok_or(Error::NotFound("the bot has no live token"))?;
    audit(
        &mut tx,
        issuer,
        "bot.token.revoke",
        bot,
        Permissions::empty(),
    )
    .await?;
    tx.commit().await.map_err(db::Error::from)?;
    Ok(old)
}

/// Every bot account, by id, with its live token.
pub async fn list_accounts(
    pool: &db::Pool,
) -> Result<Vec<(db::User, Option<db::BotToken>)>, Error> {
    let mut conn = pool.read().await?;
    let mut live: HashMap<Id, db::BotToken> = bots::live(&mut conn)
        .await?
        .into_iter()
        .map(|token| (token.bot_id, token))
        .collect();
    let mut accounts: Vec<db::User> = users::list(&mut conn)
        .await?
        .into_iter()
        .filter(|user| user.kind == UserKind::Bot)
        .collect();
    accounts.sort_by_key(|user| user.id);
    Ok(accounts
        .into_iter()
        .map(|user| {
            let token = live.remove(&user.id);
            (user, token)
        })
        .collect())
}

/// The bot account named `username`, for the operator CLI.
pub async fn find(pool: &db::Pool, username: &str) -> Result<db::User, Error> {
    let mut conn = pool.read().await?;
    users::get_by_username(&mut conn, username)
        .await?
        .filter(|user| user.kind == UserKind::Bot)
        .ok_or(Error::NotFound("no bot with that username"))
}

async fn issue(
    conn: &mut sqlx::SqliteConnection,
    bot: Id,
    token_hash: String,
    scopes: Permissions,
    actor: Option<Id>,
) -> Result<db::BotToken, Error> {
    Ok(bots::insert(
        conn,
        NewBotToken {
            id: Id::generate(),
            bot_id: bot,
            token_hash,
            scopes,
            created_by: actor,
        },
    )
    .await?)
}

fn display(name: &str) -> Result<String, Error> {
    crate::text::name(name, MAX_DISPLAY).map_err(|error| match error {
        crate::text::NameError::Length => Error::Invalid("display name is too long"),
        crate::text::NameError::Hidden => Error::Invalid("display name is not valid"),
    })
}

fn wire_bot(user: &db::User, token: Option<&db::BotToken>, plaintext: Option<String>) -> v1::Bot {
    v1::Bot {
        user: Some(v1::User {
            id: id_to_bytes(user.id),
            username: user.username.clone(),
            display_name: user.display_name.clone(),
            avatar_attachment_id: user.avatar_attachment_id.map(id_to_bytes),
            kind: v1::UserKind::Bot.into(),
        }),
        scopes: token.map_or(0, |token| token.scopes.bits()),
        active: token.is_some(),
        token_created_by: token
            .and_then(|token| token.created_by)
            .map_or_else(bytes::Bytes::new, id_to_bytes),
        token_created_at: token.map_or(0, |token| token.created_at.as_millis()),
        token: plaintext,
    }
}

async fn audit(
    conn: &mut sqlx::SqliteConnection,
    actor: Option<Id>,
    action: &str,
    bot: Id,
    scopes: Permissions,
) -> Result<(), Error> {
    let details = serde_json::json!({ "scopes": scopes.bits().to_string() }).to_string();
    audit_log::insert(
        conn,
        NewAuditEntry {
            id: Id::generate(),
            actor_id: actor,
            action: action.to_owned(),
            target_kind: "user".to_owned(),
            target_id: Some(bot),
            details,
        },
    )
    .await?;
    Ok(())
}
