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
//! gate (a leave only after its arrival). It ends when the token is revoked,
//! when the bot stops being able to sign in, when it falls behind the bus,
//! or after [`STREAM_LIFETIME`].

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
use crate::{Error, auth, events::ServerEvent, handlers::bots::StreamSlot, snapshot};

/// How long one event stream lives before the client must reconnect. Below
/// the public listener's connection age, so the stream ends cleanly.
pub(crate) const STREAM_LIFETIME: Duration = Duration::from_mins(9);
/// How often a stream rechecks its token and account, for changes another
/// process (the operator CLI) or a ban made.
const RECHECK_EVERY: Duration = Duration::from_secs(30);
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
    /// frames arrive on, `Welcome` first. `None` when the bot has every
    /// slot in use.
    pub(crate) fn bot_events(
        self: &Arc<Self>,
        bot: auth::Authenticated,
        filter: StreamFilter,
    ) -> Option<mpsc::Receiver<v1::ServerEnvelope>> {
        let slot = self.bots.open_stream(bot.session_id)?;
        let (outbound, receiver) = mpsc::channel(STREAM_QUEUE);
        let server = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = server.stream(bot, filter, slot, outbound).await {
                tracing::debug!(%error, "bot event stream ended");
            }
        });
        Some(receiver)
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
                changed = slot.revoked.changed() => {
                    if changed.is_err() || *slot.revoked.borrow() {
                        return ended(&outbound, revoked()).await;
                    }
                }
                _ = recheck.tick() => {
                    if !self.still_signed_in(bot.session_id, user).await {
                        return ended(&outbound, revoked()).await;
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

    /// The token row is still live and the account may still sign in.
    async fn still_signed_in(&self, token: Id, bot: Id) -> bool {
        match self.auth.bot_token_live(token, bot).await {
            Ok(live) => live,
            // A database hiccup does not end a stream; the next check decides.
            Err(error) => {
                tracing::debug!(%error, "bot token recheck failed");
                true
            }
        }
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

fn revoked() -> v1::ServerEnvelope {
    event_frame(server_envelope::Msg::SessionRevoked(v1::SessionRevoked {
        reason: "bot token revoked".into(),
    }))
}

fn notice(text: &str) -> v1::ServerEnvelope {
    event_frame(server_envelope::Msg::ServerNotice(v1::ServerNotice {
        text: text.to_owned(),
    }))
}
