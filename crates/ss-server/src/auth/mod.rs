//! Registration, login, sessions, and the one-time owner invite.
//!
//! Password hashing lives in [`password`] and always runs off the runtime.
//! The owner code is a `server_settings` row because an `invites` row cannot
//! exist before the first user (`created_by` is required).

mod limiter;
mod password;
mod token;

use std::{net::IpAddr, sync::Arc, time::Duration};

use sqlx::Connection;
use ss_core::{Id, Permissions, Timestamp, UserKind};
use ss_proto::v1::{self, ErrorCode};

use crate::db::{
    self, ADMIN_ROLE_ID, NewSession, NewUser, invites, roles, sessions, settings, users,
};

/// `server_settings` key for the unused owner bootstrap code.
pub const OWNER_INVITE: &str = "owner_invite";

/// Longest accepted username, in bytes. Usernames are ASCII.
const MAX_USERNAME_BYTES: usize = 32;
/// Longest accepted password, in bytes. Longer logins fail without hashing.
const MAX_PASSWORD_BYTES: usize = 128;

/// Ninety days, in milliseconds. Presenting the token pushes expiry forward.
const SESSION_TTL_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// A failure a `Hello` handler can turn into a control-stream error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Username or password did not match, or the token is unknown or expired.
    #[error("invalid username or password")]
    Unauthenticated,
    /// The account exists and the password matched, but it cannot log in.
    #[error("account disabled")]
    Disabled,
    /// An active ban. The message includes the expiry timestamp when there is one.
    #[error("{message}")]
    Banned {
        /// Text placed on the wire. The proto `Error` has no expiry field.
        message: String,
    },
    /// This address used its login or register attempts, or it already
    /// failed against a username that is locked.
    #[error("too many login attempts")]
    RateLimited {
        /// How long until the next attempt is allowed.
        retry_after: Duration,
    },
    /// The invite is missing, revoked, expired, or already used up.
    #[error("invite is not valid")]
    InvalidInvite,
    /// The username is already taken.
    #[error("username is not available")]
    Conflict,
    /// The username or password failed a format rule.
    #[error("{0}")]
    Invalid(&'static str),
    /// `set_password` was asked for an unknown username.
    #[error("user not found")]
    NotFound,
    /// `invite create --owner` was used after a user already exists.
    #[error("an owner is already registered")]
    OwnerExists,
    /// SQLite failed.
    #[error(transparent)]
    Database(#[from] db::Error),
    /// Argon2 could not hash or parse a PHC string.
    #[error("password hasher failed")]
    Password,
    /// The operating system random source failed.
    #[error("randomness failed")]
    Random,
    /// The blocking password task panicked.
    #[error("blocking task failed")]
    Join,
}

impl Error {
    /// Protocol code for this failure.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::Unauthenticated | Self::Disabled => ErrorCode::Unauthenticated,
            Self::Banned { .. } => ErrorCode::Banned,
            Self::RateLimited { .. } => ErrorCode::RateLimited,
            Self::InvalidInvite | Self::Invalid(_) => ErrorCode::Invalid,
            Self::Conflict => ErrorCode::Conflict,
            Self::NotFound => ErrorCode::NotFound,
            Self::OwnerExists | Self::Database(_) | Self::Password | Self::Random | Self::Join => {
                ErrorCode::Internal
            }
        }
    }

    /// How long a rate-limited caller should wait. `None` for other failures.
    pub(crate) fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => Some(*retry_after),
            _ => None,
        }
    }

    /// Message safe to send to the peer. Database and hasher details stay in logs.
    pub(crate) fn wire_message(&self) -> String {
        match self {
            Self::Database(_) | Self::Password | Self::Random | Self::Join => {
                "internal error".to_owned()
            }
            other => other.to_string(),
        }
    }
}

/// An account that just passed `Hello`, plus the token to send in `Welcome`.
/// `Debug` redacts the token and the password hash.
#[derive(Clone, PartialEq, Eq)]
pub struct Authenticated {
    /// Account row. The password hash is still on the struct; do not log it.
    pub user: db::User,
    /// Plaintext session token. Login and register mint a new one. Token reuse echoes it.
    pub resume_token: String,
    /// OR of the user's roles and `@everyone` at Hello.
    ///
    /// Channel checks use [`crate::perms`]. This value is not refreshed when
    /// a role changes.
    pub permissions: Permissions,
}

impl std::fmt::Debug for Authenticated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authenticated")
            .field("user", &self.user)
            .field("resume_token", &db::REDACTED)
            .field("permissions", &self.permissions)
            .finish()
    }
}

/// Password login, registration, and session lookup for one process.
pub struct Auth {
    pool: Arc<db::Pool>,
    limiter: limiter::LoginLimiter,
}

impl Auth {
    /// Limiter state starts empty. `pool` is shared with the rest of the server.
    pub fn new(pool: Arc<db::Pool>) -> Self {
        Self {
            pool,
            limiter: limiter::LoginLimiter::new(),
        }
    }

    /// Handles one `Hello` auth oneof. `peer` is the address the quota is keyed by.
    pub async fn hello(&self, hello: &v1::Hello, peer: IpAddr) -> Result<Authenticated, Error> {
        match hello.auth.as_ref() {
            Some(v1::hello::Auth::Register(register)) => self.register(hello, register, peer).await,
            Some(v1::hello::Auth::Login(login)) => self.login(hello, login, peer).await,
            Some(v1::hello::Auth::SessionToken(token) | v1::hello::Auth::ResumeToken(token)) => {
                self.resume(token).await
            }
            None => Err(Error::Invalid("missing credentials")),
        }
    }

    /// Resolves a bearer session token and slides its expiry.
    pub async fn authenticate_bearer(&self, token: &str) -> Result<Authenticated, Error> {
        self.resume(token).await
    }

    /// Display name from settings, or `Sideband` when the row is missing.
    pub(crate) async fn server_name(&self) -> String {
        let Ok(mut conn) = self.pool.read().await else {
            return "Sideband".to_owned();
        };
        settings::get(&mut conn, settings::NAME)
            .await
            .ok()
            .flatten()
            .map(|setting| setting.value)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Sideband".to_owned())
    }

    fn limit(&self, peer: IpAddr) -> Result<(), Error> {
        self.limiter
            .check(peer)
            .map_err(|retry_after| Error::RateLimited { retry_after })
    }

    async fn register(
        &self,
        hello: &v1::Hello,
        register: &v1::Register,
        peer: IpAddr,
    ) -> Result<Authenticated, Error> {
        validate_username(&register.username)?;
        validate_password(&register.password)?;
        self.limit(peer)?;
        let kind = {
            let mut conn = self.pool.read().await?;
            classify(&mut conn, &register.invite_code, Timestamp::now()).await?
        };
        let password_hash = password::hash(&register.password).await?;
        let pending = Pending {
            username: register.username.clone(),
            password_hash,
            issued: token::issue()?,
            device_name: device_name(hello),
            kind,
            invite_code: register.invite_code.clone(),
        };
        self.insert_registration(pending).await
    }

    async fn login(
        &self,
        hello: &v1::Hello,
        login: &v1::Login,
        peer: IpAddr,
    ) -> Result<Authenticated, Error> {
        if login.username.len() > MAX_USERNAME_BYTES || login.password.len() > MAX_PASSWORD_BYTES {
            return Err(Error::Unauthenticated);
        }
        self.limit(peer)?;
        self.limiter
            .admit(&login.username, peer)
            .map_err(|retry_after| Error::RateLimited { retry_after })?;
        let found = {
            let mut conn = self.pool.read().await?;
            users::get_by_username(&mut conn, &login.username).await?
        };
        let verified = match found {
            Some(user) => password::verify(&login.password, &user.password_hash)
                .await?
                .then_some(user),
            None => None,
        };
        let Some(user) = verified else {
            self.limiter.failed(&login.username, peer);
            return Err(Error::Unauthenticated);
        };
        self.limiter.succeeded(&login.username, peer);
        self.start_session(user.id, device_name(hello)).await
    }

    async fn resume(&self, token: &str) -> Result<Authenticated, Error> {
        if token.is_empty() || token.len() > 512 {
            return Err(Error::Unauthenticated);
        }
        let digest = token::hash(token);
        // Look the token up on a read connection first. An unknown token must
        // not queue behind (or hold up) the single SQLite writer.
        let found = {
            let mut conn = self.pool.read().await?;
            sessions::get_by_token_hash(&mut conn, &digest).await?
        };
        let Some(session) = found else {
            return Err(Error::Unauthenticated);
        };
        let now = Timestamp::now();
        if session.expires_at.is_some_and(|expires| expires <= now) {
            return Err(Error::Unauthenticated);
        }
        let mut conn = self.pool.write().await;
        let user = load_active(&mut conn, session.user_id).await?;
        sessions::slide(&mut conn, session.id, now, expiry(now)).await?;
        let permissions = permissions_for(&mut conn, user.id).await?;
        Ok(Authenticated {
            user,
            resume_token: token.to_owned(),
            permissions,
        })
    }

    async fn start_session(
        &self,
        user_id: Id,
        device_name: String,
    ) -> Result<Authenticated, Error> {
        let issued = token::issue()?;
        let mut conn = self.pool.write().await;
        let user = load_active(&mut conn, user_id).await?;
        let now = Timestamp::now();
        sessions::insert(
            &mut conn,
            NewSession {
                id: Id::generate(),
                user_id: user.id,
                token_hash: issued.hash,
                device_name,
                expires_at: Some(expiry(now)),
            },
        )
        .await?;
        let permissions = permissions_for(&mut conn, user.id).await?;
        Ok(Authenticated {
            user,
            resume_token: issued.token.to_string(),
            permissions,
        })
    }

    async fn insert_registration(&self, pending: Pending) -> Result<Authenticated, Error> {
        let mut guard = self.pool.write().await;
        let mut tx = guard.begin().await.map_err(db::Error::from)?;
        ensure_distinct(&mut tx, &pending.username).await?;
        let owner = claim_invite(&mut tx, &pending).await?;
        let display_name = pending.username.clone();
        let user = users::insert(
            &mut tx,
            NewUser {
                id: Id::generate(),
                username: pending.username,
                display_name,
                password_hash: pending.password_hash,
                kind: UserKind::User,
            },
        )
        .await
        .map_err(|error| match error {
            db::Error::Conflict => Error::Conflict,
            other => other.into(),
        })?;
        if owner {
            roles::assign(&mut tx, user.id, ADMIN_ROLE_ID).await?;
        }
        let now = Timestamp::now();
        sessions::insert(
            &mut tx,
            NewSession {
                id: Id::generate(),
                user_id: user.id,
                token_hash: pending.issued.hash,
                device_name: pending.device_name,
                expires_at: Some(expiry(now)),
            },
        )
        .await?;
        let permissions = permissions_for(&mut tx, user.id).await?;
        tx.commit().await.map_err(db::Error::from)?;
        Ok(Authenticated {
            user,
            resume_token: pending.issued.token.to_string(),
            permissions,
        })
    }
}

/// Prints or reuses the owner code when the database has no users.
///
/// A leftover code is deleted once any user exists, so it cannot grant `Admin`
/// to a later registration.
pub async fn ensure_owner_invite(pool: &db::Pool) -> Result<Option<String>, Error> {
    let mut conn = pool.write().await;
    if users::count(&mut conn).await? > 0 {
        if settings::get(&mut conn, OWNER_INVITE).await?.is_some() {
            settings::delete(&mut conn, OWNER_INVITE).await?;
        }
        return Ok(None);
    }
    if let Some(setting) = settings::get(&mut conn, OWNER_INVITE).await? {
        return Ok(Some(setting.value));
    }
    let code = token::owner_code()?;
    settings::set(&mut conn, OWNER_INVITE, &code).await?;
    Ok(Some(code))
}

/// Replaces the owner code. Fails once any user exists.
pub async fn create_owner_invite(pool: &db::Pool) -> Result<String, Error> {
    let code = token::owner_code()?;
    let mut conn = pool.write().await;
    if users::count(&mut conn).await? > 0 {
        return Err(Error::OwnerExists);
    }
    settings::set(&mut conn, OWNER_INVITE, &code).await?;
    Ok(code)
}

/// A new unguessable invite or webhook token.
pub(crate) fn invite_code() -> Result<String, Error> {
    token::owner_code()
}

/// Hex SHA-256 of a webhook token. The plaintext is not stored.
pub(crate) fn hash_token(token: &str) -> String {
    token::hash(token)
}

/// Replaces `username`'s password hash. The username match is case-insensitive.
pub async fn set_password(pool: &db::Pool, username: &str, password: &str) -> Result<(), Error> {
    validate_password(password)?;
    let password_hash = password::hash(password).await?;
    let mut conn = pool.write().await;
    let user = users::get_by_username(&mut conn, username)
        .await?
        .ok_or(Error::NotFound)?;
    users::set_password(&mut conn, user.id, &password_hash).await?;
    Ok(())
}

struct Pending {
    username: String,
    password_hash: String,
    issued: token::Issued,
    device_name: String,
    kind: InviteKind,
    invite_code: String,
}

enum InviteKind {
    Owner,
    Regular(Id),
}

async fn claim_invite(conn: &mut sqlx::SqliteConnection, pending: &Pending) -> Result<bool, Error> {
    match pending.kind {
        InviteKind::Owner => claim_owner(conn, &pending.invite_code).await,
        InviteKind::Regular(id) => {
            if invites::consume(conn, id, Timestamp::now()).await? {
                Ok(false)
            } else {
                Err(Error::InvalidInvite)
            }
        }
    }
}

async fn claim_owner(conn: &mut sqlx::SqliteConnection, code: &str) -> Result<bool, Error> {
    if users::count(conn).await? > 0 || !settings::take(conn, OWNER_INVITE, code).await? {
        return Err(Error::InvalidInvite);
    }
    Ok(true)
}

async fn classify(
    conn: &mut sqlx::SqliteConnection,
    code: &str,
    now: Timestamp,
) -> Result<InviteKind, Error> {
    if code.is_empty() {
        return Err(Error::InvalidInvite);
    }
    if let Some(setting) = settings::get(conn, OWNER_INVITE).await?
        && setting.value == code
    {
        return Ok(InviteKind::Owner);
    }
    let Some(invite) = invites::get_by_code(conn, code).await? else {
        return Err(Error::InvalidInvite);
    };
    let expired = invite.expires_at.is_some_and(|expires| expires <= now);
    let exhausted = invite.max_uses.is_some_and(|max| invite.uses >= max);
    if invite.revoked_at.is_some() || expired || exhausted {
        return Err(Error::InvalidInvite);
    }
    Ok(InviteKind::Regular(invite.id))
}

async fn load_active(conn: &mut sqlx::SqliteConnection, user_id: Id) -> Result<db::User, Error> {
    let user = users::get(conn, user_id)
        .await?
        .ok_or(Error::Unauthenticated)?;
    if user.disabled_at.is_some() {
        return Err(Error::Disabled);
    }
    if let Some(ban) = active_ban(conn, user.id).await? {
        return Err(banned(ban.expires_at));
    }
    Ok(user)
}

async fn active_ban(
    conn: &mut sqlx::SqliteConnection,
    user_id: Id,
) -> Result<Option<db::Ban>, Error> {
    let now = Timestamp::now();
    let bans = crate::db::bans::list_for_user(conn, user_id).await?;
    Ok(bans
        .into_iter()
        .find(|ban| ban.lifted_at.is_none() && ban.expires_at.is_none_or(|expires| expires > now)))
}

async fn permissions_for(
    conn: &mut sqlx::SqliteConnection,
    user_id: Id,
) -> Result<Permissions, Error> {
    let roles = roles::roles_for_user(conn, user_id).await?;
    let mut permissions = Permissions::EVERYONE;
    for role in roles {
        permissions |= role.permissions;
    }
    Ok(permissions)
}

fn banned(expires_at: Option<Timestamp>) -> Error {
    let message = match expires_at {
        Some(expires) => format!("banned until {}", expires.as_millis()),
        None => "banned".to_owned(),
    };
    Error::Banned { message }
}

const fn expiry(now: Timestamp) -> Timestamp {
    Timestamp::from_millis(now.as_millis().saturating_add(SESSION_TTL_MS))
}

/// Rejects a username that reads like an existing one, such as `rnike` next
/// to `mike` or `adm1n` next to `admin`. Exact (case-insensitive) duplicates
/// are left to the `UNIQUE COLLATE NOCASE` index.
async fn ensure_distinct(conn: &mut sqlx::SqliteConnection, username: &str) -> Result<(), Error> {
    let wanted = username_skeleton(username);
    let taken = users::list(conn)
        .await?
        .iter()
        .any(|user| username_skeleton(&user.username) == wanted);
    if taken { Err(Error::Conflict) } else { Ok(()) }
}

/// Folds ASCII look-alikes so confusable usernames compare equal.
///
/// Usernames are restricted to `a-z 0-9 _ .`, so NFC normalization is the
/// identity on every accepted name and only these ASCII confusables remain:
/// `rn`/`m`, `vv`/`w`, `cl`/`d`, `0`/`o`, `1`/`i`/`l`, and `_`/`.`.
#[must_use]
pub fn username_skeleton(username: &str) -> String {
    let lower = username.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let folded_pair = match bytes.get(index..index.saturating_add(2)) {
            Some(b"rn") => Some('m'),
            Some(b"vv") => Some('w'),
            Some(b"cl") => Some('d'),
            _ => None,
        };
        if let Some(folded) = folded_pair {
            out.push(folded);
            index = index.saturating_add(2);
            continue;
        }
        out.push(match byte {
            b'0' => 'o',
            b'1' | b'i' => 'l',
            b'_' => '.',
            other => char::from(other),
        });
        index = index.saturating_add(1);
    }
    out
}

fn validate_username(username: &str) -> Result<(), Error> {
    let length = username.chars().count();
    let charset = username.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.')
    });
    if (3..=MAX_USERNAME_BYTES).contains(&length) && charset {
        Ok(())
    } else {
        Err(Error::Invalid(
            "username must be 3-32 characters of a-z, 0-9, underscore, or dot",
        ))
    }
}

fn validate_password(password: &str) -> Result<(), Error> {
    if (8..=MAX_PASSWORD_BYTES).contains(&password.len()) {
        Ok(())
    } else {
        Err(Error::Invalid("password must be 8 to 128 bytes"))
    }
}

fn device_name(hello: &v1::Hello) -> String {
    let name = hello.client_name.trim();
    if name.is_empty() {
        "unknown".to_owned()
    } else {
        name.chars().take(64).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{username_skeleton, validate_username};

    #[test]
    fn confusable_usernames_share_a_skeleton() {
        for (left, right) in [
            ("mike", "rnike"),
            ("admin", "adm1n"),
            ("admin", "admln"),
            ("bob", "b0b"),
            ("wendy", "vvendy"),
            ("dave", "clave"),
            ("john.doe", "john_doe"),
            ("Alice", "alice"),
        ] {
            assert_eq!(
                username_skeleton(left),
                username_skeleton(right),
                "{left} {right}"
            );
        }
        for (left, right) in [("mike", "mika"), ("bob", "bobby"), ("ann", "anna")] {
            assert_ne!(
                username_skeleton(left),
                username_skeleton(right),
                "{left} {right}"
            );
        }
    }

    #[test]
    fn usernames_are_short_ascii() {
        assert!(validate_username("ada.l_33").is_ok());
        for bad in [
            "ab",
            "Ada",
            "ad a",
            "caf\u{e9}",
            "cafe\u{301}",
            &"a".repeat(33),
        ] {
            assert!(validate_username(bad).is_err(), "{bad:?}");
        }
    }
}
