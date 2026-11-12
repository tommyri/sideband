//! The bot API's HTTPS side of a session (T-080).
//!
//! An action ([`Server::bot_command`]) runs the same dispatch a QUIC command
//! runs: the per-account command budget, the per-bot budget, the handler and
//! its checks, then publishing. It runs on its own task, so a client that
//! hangs up cannot leave a commit without its event.
//!
//! An event stream ([`Server::bot_events`]) starts with a `Welcome` and its
//! bus subscription at the snapshot's read point, then forwards bus events
//! through the filters a QUIC session uses: [`snapshot::delivers`], the DM
//! membership rules, the debounced guild channel diff, and the voice-state
//! gate (a leave only after its arrival). It ends when the token is revoked
//! (and never opens on one revoked before it took its slot), when the bot is
//! kicked or banned, when its account can no longer sign in, when it falls
//! behind the bus, or after [`STREAM_LIFETIME`].

use std::{collections::HashSet, sync::Arc, time::Duration};

use ss_core::Id;
use ss_proto::{
    v1::{self, client_envelope::Msg, server_envelope},
    validate::id_from_bytes,
};
use tokio::{
    sync::{broadcast, mpsc},
    time::{Instant, timeout},
};

use super::{
    AuthedContext, Server, View, announce_direct, apply_direct, channel_event, direct_channel,
    event_frame, publish, refreshes_guild, replies, sync_channels, welcome_envelope,
};
use crate::{
    Error, auth,
    events::ServerEvent,
    handlers::bots::{StreamRefused, StreamSlot, revoked_frame},
    snapshot,
};

/// How long one event stream lives before the client must reconnect. Below
/// the public listener's connection age, so the stream ends cleanly.
pub(crate) const STREAM_LIFETIME: Duration = Duration::from_mins(9);
/// How often a stream rechecks its token and account, for changes another
/// process (the operator CLI) or a ban made.
const RECHECK_EVERY: Duration = Duration::from_secs(30);
/// Wait before the one retry of a recheck that failed.
const RECHECK_RETRY: Duration = Duration::from_secs(1);
/// Frames a stream may hold for a slow client before it waits.
pub(crate) const STREAM_QUEUE: usize = 32;

/// What a stream forwards beyond the visibility filters.
#[derive(Clone, Default)]
pub(crate) struct StreamFilter {
    /// Only `MessageCreated` whose content starts with this. Other events
    /// pass unchanged.
    pub prefix: Option<String>,
}

impl Server {
    /// Signs a bot in from an HTTPS `Bot` token. Session tokens never match.
    ///
    /// The permission snapshot caps a bot by its live token. When it names
    /// another token (the operator CLI created or replaced it from another
    /// process), it is reloaded first, so the bot is never checked against a
    /// snapshot that does not cap it by this token's scopes.
    pub(crate) async fn bot_caller(&self, token: &str) -> Result<auth::Authenticated, auth::Error> {
        let bot = self.auth.authenticate_bot(token).await?;
        if self.perms.bot_token(bot.user.id) != Some(bot.session_id) {
            self.perms.reload().await?;
            // Revoked again meanwhile: fail closed rather than run uncapped.
            if self.perms.bot_token(bot.user.id) != Some(bot.session_id) {
                return Err(auth::Error::Unauthenticated);
            }
        }
        Ok(bot)
    }

    /// The per-bot budget, for HTTPS reads that are not commands.
    pub(crate) fn spend_bot(&self, bot: Id) -> Result<(), Duration> {
        self.bots.spend(bot)
    }

    /// Permission snapshot, for HTTPS reads.
    pub(crate) fn permissions(&self) -> &crate::perms::PermissionService {
        &self.perms
    }

    /// Runs `msg` for `bot` as one command and returns its `Ack` or `Error`
    /// envelope (request id 1). Its events are published to every session
    /// and stream, the bot's own included.
    pub(crate) async fn bot_command(
        self: &Arc<Self>,
        bot: &auth::Authenticated,
        msg: Msg,
    ) -> v1::ServerEnvelope {
        const REQUEST: u32 = 1;
        if let Err(wait) = self.commands.check(&bot.user.id) {
            return v1::ServerEnvelope::rate_limited(REQUEST, "too many commands", wait);
        }
        let context = AuthedContext {
            user_id: bot.user.id,
            // A fresh origin, so no session skips the bus copy of these events.
            conn_id: Id::generate(),
            session_id: bot.session_id,
            perms_cache: bot.permissions,
        };
        let envelope = v1::ClientEnvelope {
            request_id: REQUEST,
            msg: Some(msg),
        };
        let server = Arc::clone(self);
        let task = tokio::spawn(async move {
            let answer = replies(&server, &context, &envelope).await;
            publish(&server, &context, answer.events.clone());
            answer
        });
        match task.await {
            Ok(answer) => answer
                .frames
                .into_iter()
                .find(|frame| frame.request_id == REQUEST)
                .unwrap_or_else(|| {
                    v1::ServerEnvelope::error(REQUEST, v1::ErrorCode::Internal, "internal error")
                }),
            Err(error) => {
                tracing::warn!(%error, "bot command task failed");
                v1::ServerEnvelope::error(REQUEST, v1::ErrorCode::Internal, "internal error")
            }
        }
    }

    /// Opens an event stream for `bot`: a slot (at most
    /// [`crate::limits::BOT_STREAMS`] per token) and the receiver its
    /// frames arrive on, `Welcome` first.
    ///
    /// The slot is taken before the token is checked again in the database:
    /// a revoke that committed after `bot` signed in is seen by that check,
    /// and one that commits later signals the slot. Either way no `Welcome`
    /// goes out on a dead token.
    pub(crate) async fn bot_events(
        self: &Arc<Self>,
        bot: auth::Authenticated,
        filter: StreamFilter,
    ) -> Result<mpsc::Receiver<v1::ServerEnvelope>, StreamRefused> {
        #[cfg(test)]
        self.bots.gate.pass().await;
        let slot = self.bots.open_stream(bot.user.id, bot.session_id)?;
        match self.still_signed_in(bot.session_id, bot.user.id).await {
            Recheck::Live => {}
            Recheck::Dead => return Err(StreamRefused::Revoked),
            Recheck::Unknown => return Err(StreamRefused::Unavailable),
        }
        let (outbound, receiver) = mpsc::channel(STREAM_QUEUE);
        let server = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = server.stream(bot, filter, slot, outbound).await {
                tracing::debug!(%error, "bot event stream ended");
            }
        });
        Ok(receiver)
    }

    async fn stream(
        &self,
        mut bot: auth::Authenticated,
        filter: StreamFilter,
        mut slot: StreamSlot,
        outbound: mpsc::Sender<v1::ServerEnvelope>,
    ) -> Result<(), Error> {
        let user = bot.user.id;
        let context = AuthedContext {
            user_id: user,
            conn_id: Id::generate(),
            session_id: bot.session_id,
            perms_cache: bot.permissions,
        };
        let (mut snapshot, mut subscription) =
            snapshot::build_live(&self.perms, &self.bus, user, |id| {
                self.registry.presence(id)
            })
            .await
            .map_err(|error| {
                tracing::debug!(%error, "bot snapshot failed");
                Error::Protocol
            })?;
        self.voice.fill(&mut snapshot, user);
        let mut seen: HashSet<Id> = snapshot
            .voice_states
            .iter()
            .filter_map(|state| id_from_bytes(&state.conn_id).ok())
            .collect();
        // Ended while the snapshot was read: the last frame instead of it.
        let last = slot.end.borrow().clone();
        if let Some(last) = last {
            return ended(&outbound, last).await;
        }
        let server_name = self.auth.server_name().await;
        // The stream does not hand the token back.
        bot.resume_token.clear();
        let welcome = welcome_envelope(0, &bot, context.conn_id, &server_name, snapshot);
        let mut view = View::from_welcome(&welcome);
        send(&outbound, welcome).await?;
        let ends = Instant::now() + STREAM_LIFETIME;
        let mut recheck = tokio::time::interval_at(Instant::now() + RECHECK_EVERY, RECHECK_EVERY);
        loop {
            // A revoke wins over an event that is ready at the same time.
            tokio::select! {
                biased;
                () = outbound.closed() => return Ok(()),
                changed = slot.end.changed() => {
                    let last = slot.end.borrow().clone();
                    match (changed, last) {
                        (_, Some(last)) => return ended(&outbound, last).await,
                        (Err(_), None) => return ended(&outbound, revoked_frame()).await,
                        (Ok(()), None) => {}
                    }
                }
                _ = recheck.tick() => {
                    // Fails closed: unconfirmed ends the stream too, but as a
                    // notice, so a bot with a live token reconnects.
                    match self.still_signed_in(bot.session_id, user).await {
                        Recheck::Live => {}
                        Recheck::Dead => return ended(&outbound, revoked_frame()).await,
                        Recheck::Unknown => {
                            return ended(&outbound, notice("could not confirm the bot token; reconnect")).await;
                        }
                    }
                }
                () = tokio::time::sleep_until(ends) => {
                    return ended(&outbound, notice("event stream lifetime reached; reconnect")).await;
                }
                event = subscription.recv() => match event {
                    Ok(event) => {
                        self.forward(&context, &mut view, &mut seen, &filter, &outbound, &event).await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        return ended(&outbound, notice("event stream fell behind; reconnect")).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
                () = super::due(view.refresh_at) => {
                    sync_channels(self, &context, &mut view, &outbound).await?;
                }
            }
        }
    }

    /// Whether the token row is still live and the account may still sign
    /// in. A check that errors is retried once, a second later; a second
    /// error is [`Recheck::Unknown`], which callers treat as not signed in.
    async fn still_signed_in(&self, token: Id, bot: Id) -> Recheck {
        for attempt in 0..2 {
            match self.auth.bot_token_live(token, bot).await {
                Ok(true) => return Recheck::Live,
                Ok(false) => return Recheck::Dead,
                Err(error) => {
                    tracing::debug!(%error, attempt, "bot token recheck failed");
                    if attempt == 0 {
                        tokio::time::sleep(RECHECK_RETRY).await;
                    }
                }
            }
        }
        Recheck::Unknown
    }

    /// [`super::on_event`] for a stream: the same filters, with the voice
    /// gate kept here because a stream is not a registered connection.
    async fn forward(
        &self,
        context: &AuthedContext,
        view: &mut View,
        seen: &mut HashSet<Id>,
        filter: &StreamFilter,
        outbound: &mpsc::Sender<v1::ServerEnvelope>,
        event: &ServerEvent,
    ) -> Result<(), Error> {
        let msg = &*event.msg;
        if direct_channel(msg).is_some() {
            return apply_direct(self, context, view, outbound, msg).await;
        }
        if refreshes_guild(msg) {
            view.refresh_at
                .get_or_insert_with(|| Instant::now() + super::REFRESH_DEBOUNCE);
        }
        if channel_event(msg) {
            return Ok(());
        }
        if let Some(channel) = snapshot::event_channel(msg) {
            if view.refresh_at.is_some() {
                sync_channels(self, context, view, outbound).await?;
            }
            announce_direct(self, context, view, outbound, channel).await?;
        }
        if !snapshot::delivers(&self.perms, context.user_id, msg)
            || !admit_voice(seen, msg)
            || !filter.passes(msg)
        {
            return Ok(());
        }
        send(outbound, event_frame(msg.clone())).await
    }
}

/// The outcome of rechecking a stream's token.
enum Recheck {
    Live,
    Dead,
    /// The check failed twice; the stream is not kept.
    Unknown,
}

impl StreamFilter {
    fn passes(&self, msg: &server_envelope::Msg) -> bool {
        match (&self.prefix, msg) {
            (Some(prefix), server_envelope::Msg::MessageCreated(created)) => created
                .message
                .as_ref()
                .is_some_and(|message| message.content.starts_with(prefix.as_str())),
            _ => true,
        }
    }
}

/// The registry's voice-state gate for a stream: a channel-less
/// `VoiceStateUpdated` (a leave) only for a connection it was told joined.
fn admit_voice(seen: &mut HashSet<Id>, msg: &server_envelope::Msg) -> bool {
    let server_envelope::Msg::VoiceStateUpdated(state) = msg else {
        return true;
    };
    let Ok(conn) = id_from_bytes(&state.conn_id) else {
        return false;
    };
    if state.channel_id.is_some() {
        seen.insert(conn);
        true
    } else {
        seen.remove(&conn)
    }
}

/// Waits for room, but not past the slow-consumer grace: a stream that
/// cannot keep up ends rather than skipping a frame.
async fn send(
    outbound: &mpsc::Sender<v1::ServerEnvelope>,
    frame: v1::ServerEnvelope,
) -> Result<(), Error> {
    match timeout(crate::registry::SLOW_CONSUMER_AFTER, outbound.send(frame)).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(Error::Protocol),
    }
}

async fn ended(
    outbound: &mpsc::Sender<v1::ServerEnvelope>,
    last: v1::ServerEnvelope,
) -> Result<(), Error> {
    let _ = timeout(Duration::from_secs(1), outbound.send(last)).await;
    Ok(())
}

fn notice(text: &str) -> v1::ServerEnvelope {
    event_frame(server_envelope::Msg::ServerNotice(v1::ServerNotice {
        text: text.to_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use ss_core::Permissions;
    use tower::ServiceExt;

    use super::Server;
    use crate::{
        db,
        handlers::bots::{create_account, revoke_token},
    };

    async fn server() -> (tempfile::TempDir, Arc<db::Pool>, Arc<Server>) {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(db::Pool::open(dir.path().join("t.db")).await.unwrap());
        let server = Arc::new(Server::open(Arc::clone(&pool)).await.unwrap());
        (dir, pool, server)
    }

    fn events(token: &str) -> Request<Body> {
        Request::builder()
            .uri("/api/v1/events")
            .header(header::AUTHORIZATION, format!("Bot {token}"))
            .body(Body::empty())
            .unwrap()
    }

    /// Signs a stream request in, then lets `revoke` run before the stream
    /// takes its slot, and returns the response status.
    async fn revoked_between_sign_in_and_slot(
        revoke: impl AsyncFnOnce(&db::Pool, &Server, ss_core::Id),
    ) -> StatusCode {
        let (_dir, pool, server) = server().await;
        let (user, _, token) = create_account(&pool, None, "racer", "", Permissions::VIEW_CHANNEL)
            .await
            .unwrap();
        let (reached, release) = server.bots.gate.arm();
        let app = crate::http::bots::router(Arc::clone(&server));
        let request = tokio::spawn(app.oneshot(events(&token)));
        reached.await.unwrap();
        revoke(&pool, &server, user.id).await;
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), request)
            .await
            .expect("the stream request did not end at once")
            .unwrap()
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn a_token_revoked_before_its_stream_takes_a_slot_gets_no_welcome() {
        // As `RevokeBotToken` does: commit, reload, then end what used it.
        let status = revoked_between_sign_in_and_slot(async |pool, server, bot| {
            let old = revoke_token(pool, None, bot).await.unwrap();
            server.perms.reload().await.unwrap();
            server.bots.end(old);
        })
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a stream opened on a dead token"
        );
    }

    #[tokio::test]
    async fn a_token_another_process_revoked_before_the_slot_gets_no_welcome() {
        // The operator CLI: the row changes, nothing in this process is told.
        let status = revoked_between_sign_in_and_slot(async |pool, _, bot| {
            let _old = revoke_token(pool, None, bot).await.unwrap();
        })
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a stream opened on a dead token"
        );
    }
}
