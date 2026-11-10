//! Live sessions: logout, self-service revoke, the operator's revoke, and
//! the timer that closes dead connections and prunes expired rows.
//!
//! A token is checked against `sessions` on every `Hello` and every HTTPS
//! request, so deleting the row ends it for new requests at once. A QUIC
//! connection authenticated earlier stays open until something closes it.
//! [`Sessions`] remembers which session each connection used: a revoke in
//! this process closes them immediately, and [`Sessions::sweep`] closes the
//! ones whose row another process (the CLI) deleted or that expired.

use std::{collections::HashMap, sync::Arc, time::Duration};

use parking_lot::Mutex;
use ss_core::{Id, Timestamp};
use ss_proto::{
    v1::{self, ack, client_envelope::Msg, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};
use tokio::sync::mpsc;

use super::{Error, SESSION_MAX_AGE_MS, session_alive};
use crate::{
    db::{self, sessions, users},
    handlers,
    registry::Kick,
};

/// QUIC close code after `Logout` or a revoke. The token is dead; clients
/// must not reconnect with it.
pub const CLOSE_SIGNED_OUT: u32 = 6;
const SIGNED_OUT: &[u8] = b"signed out";
/// How often live connections are checked against `sessions`. Bounds how
/// long a connection outlives a revoke made by another process.
const SWEEP_EVERY: Duration = Duration::from_secs(30);
/// How often expired rows are deleted.
const PRUNE_EVERY: Duration = Duration::from_hours(1);

struct Live {
    session: Id,
    kicks: mpsc::Sender<Kick>,
    quic: quinn::Connection,
}

/// Which session each authenticated connection used.
pub struct Sessions {
    pool: Arc<db::Pool>,
    live: Mutex<HashMap<Id, Live>>,
}

/// Removes a connection from [`Sessions`] when its control task ends.
pub(crate) struct Attached {
    sessions: Arc<Sessions>,
    conn: Id,
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.sessions.live.lock().remove(&self.conn);
    }
}

/// The connection a session command arrived on.
#[derive(Clone, Copy)]
pub(crate) struct Caller {
    pub user: Id,
    pub session: Id,
    pub conn: Id,
}

impl Sessions {
    /// No connections yet. `pool` is shared with the rest of the server.
    pub(crate) fn new(pool: Arc<db::Pool>) -> Self {
        Self {
            pool,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// Records that `conn` authenticated with `session`. `kicks` is the
    /// connection's writer channel, the same one the registry uses.
    pub(crate) fn attach(
        self: &Arc<Self>,
        conn: Id,
        session: Id,
        kicks: mpsc::Sender<Kick>,
        quic: quinn::Connection,
    ) -> Attached {
        self.live.lock().insert(
            conn,
            Live {
                session,
                kicks,
                quic,
            },
        );
        Attached {
            sessions: Arc::clone(self),
            conn,
        }
    }

    /// Connections currently attached to `session`.
    #[must_use]
    pub fn connections(&self, session: Id) -> usize {
        self.live
            .lock()
            .values()
            .filter(|live| live.session == session)
            .count()
    }

    /// Answers `Logout`, `ListSessions`, and `RevokeSession`. `Ok(None)` means
    /// the reply already went out as the connection's last frame.
    pub(crate) async fn dispatch(
        &self,
        caller: Caller,
        request_id: u32,
        msg: &Msg,
    ) -> Option<Result<Option<ack::Result>, handlers::Error>> {
        match msg {
            Msg::Logout(_) => Some(self.logout(caller, request_id).await.map(|()| None)),
            Msg::ListSessions(_) => Some(self.list(caller).await.map(Some)),
            Msg::RevokeSession(command) => Some(self.revoke(caller, request_id, command).await),
            _ => None,
        }
    }

    async fn logout(&self, caller: Caller, request_id: u32) -> Result<(), handlers::Error> {
        {
            let mut conn = self.pool.write().await;
            // Already gone (a sweep is pending) is still a successful logout.
            sessions::delete_owned(&mut conn, caller.session, caller.user).await?;
        }
        self.close(caller.session, Some((caller.conn, request_id)));
        Ok(())
    }

    async fn list(&self, caller: Caller) -> Result<ack::Result, handlers::Error> {
        let rows = {
            let mut conn = self.pool.read().await?;
            sessions::list_for_user(&mut conn, caller.user).await?
        };
        let now = Timestamp::now();
        let sessions = rows
            .into_iter()
            .filter(|session| session_alive(session, now))
            .map(|session| v1::SessionInfo {
                id: id_to_bytes(session.id),
                device_name: session.device_name,
                created_at: session.created_at.as_millis(),
                last_seen_at: session.last_seen_at.as_millis(),
                expires_at: session
                    .expires_at
                    .map_or(i64::MAX, Timestamp::as_millis)
                    .min(
                        session
                            .created_at
                            .as_millis()
                            .saturating_add(SESSION_MAX_AGE_MS),
                    ),
                current: session.id == caller.session,
            })
            .collect();
        Ok(ack::Result::Sessions(v1::SessionList { sessions }))
    }

    async fn revoke(
        &self,
        caller: Caller,
        request_id: u32,
        command: &v1::RevokeSession,
    ) -> Result<Option<ack::Result>, handlers::Error> {
        let target = id_from_bytes(&command.session_id)
            .map_err(|_| handlers::Error::Invalid("session_id must be 16 bytes"))?;
        if target == caller.session {
            self.logout(caller, request_id).await?;
            return Ok(None);
        }
        let deleted = {
            let mut conn = self.pool.write().await;
            // Scoped to the caller: another account's id is "not found".
            sessions::delete_owned(&mut conn, target, caller.user).await?
        };
        if !deleted {
            return Err(handlers::Error::NotFound("session not found"));
        }
        self.close(target, None);
        Ok(Some(ack::Result::Empty(v1::Empty {})))
    }

    /// Closes every connection that used `session`. `answer`, when set, is
    /// the connection and request that asked: it gets its `Ack` as the last
    /// frame instead of `SessionRevoked`.
    fn close(&self, session: Id, answer: Option<(Id, u32)>) {
        let targets: Vec<_> = self
            .live
            .lock()
            .iter()
            .filter(|(_, live)| live.session == session)
            .map(|(conn, live)| (*conn, live.kicks.clone(), live.quic.clone()))
            .collect();
        for (conn, kicks, quic) in targets {
            let notice = match answer {
                Some((asker, request_id)) if asker == conn => {
                    v1::ServerEnvelope::ack(request_id, ack::Result::Empty(v1::Empty {}))
                }
                _ => revoked_notice(),
            };
            let kick = Kick {
                notice,
                code: CLOSE_SIGNED_OUT,
                reason: SIGNED_OUT,
            };
            if kicks.try_send(kick).is_err() {
                quic.close(CLOSE_SIGNED_OUT.into(), SIGNED_OUT);
            }
        }
    }

    /// Closes connections whose session row is gone or past either lifetime.
    /// Returns how many sessions were closed. Runs every 30 s from
    /// [`Self::run`]; tests call it directly.
    pub async fn sweep(&self) -> Result<usize, Error> {
        let mut ids: Vec<Id> = self.live.lock().values().map(|live| live.session).collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return Ok(0);
        }
        let now = Timestamp::now();
        let mut dead = Vec::new();
        {
            let mut conn = self.pool.read().await?;
            for id in ids {
                let alive = sessions::get(&mut conn, id)
                    .await?
                    .is_some_and(|session| session_alive(&session, now));
                if !alive {
                    dead.push(id);
                }
            }
        }
        for id in &dead {
            self.close(*id, None);
        }
        Ok(dead.len())
    }

    /// Deletes rows past their idle or absolute lifetime. Returns how many.
    pub async fn prune(&self) -> Result<u64, Error> {
        prune(&self.pool, Timestamp::now()).await
    }

    /// Sweeps every 30 s and prunes every hour, starting now, until dropped.
    pub async fn run(self: Arc<Self>) {
        let mut sweep = tokio::time::interval(SWEEP_EVERY);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut prune = tokio::time::interval(PRUNE_EVERY);
        prune.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = sweep.tick() => match self.sweep().await {
                    Ok(0) => {}
                    Ok(closed) => tracing::info!(closed, "closed connections of ended sessions"),
                    Err(error) => tracing::warn!(%error, "session sweep failed"),
                },
                _ = prune.tick() => match self.prune().await {
                    Ok(0) => {}
                    Ok(removed) => tracing::info!(removed, "pruned expired sessions"),
                    Err(error) => tracing::warn!(%error, "session prune failed"),
                },
            }
        }
    }
}

fn revoked_notice() -> v1::ServerEnvelope {
    v1::ServerEnvelope {
        request_id: 0,
        msg: Some(server_envelope::Msg::SessionRevoked(v1::SessionRevoked {
            reason: "signed out: this session was ended".into(),
        })),
    }
}

async fn prune(pool: &db::Pool, now: Timestamp) -> Result<u64, Error> {
    let created_before = Timestamp::from_millis(now.as_millis().saturating_sub(SESSION_MAX_AGE_MS));
    let mut conn = pool.write().await;
    Ok(sessions::delete_expired(&mut conn, now, created_before).await?)
}

/// `username`'s live sessions, oldest first, for the operator CLI.
pub async fn list_sessions(pool: &db::Pool, username: &str) -> Result<Vec<db::Session>, Error> {
    let mut conn = pool.write().await;
    let user = users::get_by_username(&mut conn, username)
        .await?
        .ok_or(Error::NotFound)?;
    let now = Timestamp::now();
    Ok(sessions::list_for_user(&mut conn, user.id)
        .await?
        .into_iter()
        .filter(|session| session_alive(session, now))
        .collect())
}

/// Ends one of `username`'s sessions, or all of them when `session` is
/// `None`. Returns how many rows were deleted; `Error::NotFound` when the
/// account or that session of the account does not exist. A running server
/// closes the connections on its next sweep.
pub async fn revoke_sessions(
    pool: &db::Pool,
    username: &str,
    session: Option<Id>,
) -> Result<u64, Error> {
    let mut conn = pool.write().await;
    let user = users::get_by_username(&mut conn, username)
        .await?
        .ok_or(Error::NotFound)?;
    match session {
        None => Ok(sessions::delete_for_user(&mut conn, user.id).await?),
        Some(id) => {
            if sessions::delete_owned(&mut conn, id, user.id).await? {
                Ok(1)
            } else {
                Err(Error::NotFound)
            }
        }
    }
}
