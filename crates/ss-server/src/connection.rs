//! Authenticated control-stream owner.
//!
//! The voice spike in the crate root still accepts any username. This server
//! requires [`auth`] and then answers `Ping`, presence, profile, role, channel,
//! direct message, text, and voice commands. Events are delivered to the actor on the command reply and
//! published for every other visible session. Media datagrams are forwarded
//! only to members of the same voice channel.

use crate::{
    Error, MAX_CONNECTIONS,
    auth::{self, Auth},
    events::{EventBus, ServerEvent},
    handlers::{
        Channels, DirectMessages, Messages, Moderation, Presence, Profiles, Roles, Webhooks,
    },
    media::{self, Forwarder},
    perms::PermissionService,
    registry::{self, ClientInfo, ConnectionRegistry, Kick, Registration},
    snapshot, storage,
    voice::{self, Voice},
};
use prost::Message;
use ss_core::{Id, Permissions, Timestamp};
use ss_net::{
    Connection,
    control::{ControlError, ServerStream},
    server::Endpoint,
};
use ss_proto::{
    v1::{self, client_envelope::Msg, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinSet,
    time::timeout,
};

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const NOTICE_TIMEOUT: Duration = Duration::from_secs(1);
/// Lets the notice frame leave the socket before `CONNECTION_CLOSE`.
const NOTICE_GRACE: Duration = Duration::from_millis(100);

/// Identity and permission cache for one accepted control stream.
pub struct AuthedContext {
    /// Account that passed `Hello`.
    pub user_id: Id,
    /// This QUIC connection, distinct from the account.
    pub conn_id: Id,
    /// OR of the user's roles at Hello. Command checks use [`crate::perms`],
    /// so a role change applies on the next command without reconnecting.
    pub perms_cache: Permissions,
}

/// Accept loop that authenticates `Hello` before dispatching.
pub struct Server {
    pool: Arc<crate::db::Pool>,
    auth: Auth,
    perms: Arc<PermissionService>,
    roles: Roles,
    dms: DirectMessages,
    channels: Channels,
    messages: Messages,
    webhooks: Arc<Webhooks>,
    profiles: Profiles,
    presence: Presence,
    moderation: Moderation,
    voice: Arc<Voice>,
    media: Arc<Forwarder>,
    registry: Arc<ConnectionRegistry>,
    bus: EventBus,
    files: storage::Store,
    stop: watch::Sender<bool>,
}

struct Answer {
    frames: Vec<v1::ServerEnvelope>,
    events: Vec<server_envelope::Msg>,
}

impl Server {
    /// Loads permissions, then shares `pool` with CLI helpers and repository calls.
    pub async fn open(pool: Arc<crate::db::Pool>) -> Result<Self, crate::db::Error> {
        Self::open_with(pool, storage::UploadPolicy::default()).await
    }

    /// Like [`Self::open`], with the upload cap from configuration.
    pub async fn open_with(
        pool: Arc<crate::db::Pool>,
        policy: storage::UploadPolicy,
    ) -> Result<Self, crate::db::Error> {
        let perms = Arc::new(PermissionService::open(Arc::clone(&pool)).await?);
        let registry = Arc::new(ConnectionRegistry::new(registry::SLOW_CONSUMER_AFTER));
        let (voice, media) = Voice::open(Arc::clone(&perms));
        let (stop, _) = watch::channel(false);
        let files = storage::Store::open_with_policy(Arc::clone(&pool), policy).await?;
        let bus = EventBus::new();
        let messages = Messages::new(Arc::clone(&perms), Arc::clone(&pool), bus.clone());
        let webhooks = Arc::new(Webhooks::new(
            Arc::clone(&perms),
            messages.previews(),
            bus.clone(),
        ));
        Ok(Self {
            pool: Arc::clone(&pool),
            auth: Auth::new(Arc::clone(&pool)),
            roles: Roles::new(Arc::clone(&perms)),
            dms: DirectMessages::new(Arc::clone(&perms)),
            channels: Channels::new(Arc::clone(&perms)),
            messages,
            webhooks,
            profiles: Profiles::new(Arc::clone(&perms), Arc::clone(&registry)),
            presence: Presence::new(Arc::clone(&registry)),
            moderation: Moderation::new(
                Arc::clone(&perms),
                Arc::clone(&registry),
                Arc::clone(&voice),
            ),
            files,
            perms,
            voice,
            media,
            registry,
            bus,
            stop,
        })
    }

    /// Opaque-media counters for the authenticated router.
    pub fn media_stats(&self) -> media::Stats {
        self.media.stats()
    }

    /// Members currently sitting in a voice channel.
    #[must_use]
    pub fn voice_members(&self) -> usize {
        self.voice.member_count()
    }

    /// History, uploads, audit, and incoming webhooks. `run` binds this.
    pub fn public_routes(&self) -> axum::Router {
        let auth = Arc::new(Auth::new(Arc::clone(&self.pool)));
        let perms = Arc::clone(&self.perms);
        crate::http::messages::router(Arc::clone(&auth), Arc::clone(&perms))
            .merge(crate::http::audit::router(
                Arc::clone(&auth),
                Arc::clone(&perms),
            ))
            .merge(crate::http::attachments::router(
                auth,
                perms,
                self.files.clone(),
            ))
            .merge(self.webhook_routes())
    }

    /// Incoming-webhook routes. `run` binds HTTP; tests call this directly.
    pub fn webhook_routes(&self) -> axum::Router {
        crate::http::webhooks::router(Arc::clone(&self.webhooks))
    }

    /// File store used by upload routes. The process does not bind HTTP yet.
    #[must_use]
    pub fn files(&self) -> storage::Store {
        self.files.clone()
    }

    /// Live connections, presence, and the slow-consumer timer.
    pub fn registry(&self) -> &ConnectionRegistry {
        &self.registry
    }

    /// Notifies every connection, closes them, and stops [`Self::run`].
    pub fn shutdown(&self) {
        self.registry.shutdown();
        let _ = self.stop.send(true);
    }

    /// Accepts up to [`MAX_CONNECTIONS`] peers. Dropping this future closes them.
    pub async fn run(self: Arc<Self>, endpoint: Endpoint) {
        let mut tasks = JoinSet::new();
        let mut stop = self.stop.subscribe();
        if *stop.borrow() {
            return;
        }
        loop {
            tokio::select! {
                biased;
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else { break };
                    if tasks.len() >= MAX_CONNECTIONS {
                        incoming.refuse();
                        continue;
                    }
                    let server = Arc::clone(&self);
                    tasks.spawn(async move {
                        let result = async {
                            let connection = timeout(OPEN_TIMEOUT, incoming.accept())
                                .await
                                .map_err(|_| Error::Timeout)??;
                            server.session(connection).await
                        }
                        .await;
                        if let Err(error) = result {
                            tracing::debug!(%error, "authenticated session ended");
                        }
                    });
                }
                _ = tasks.join_next(), if !tasks.is_empty() => {}
            }
        }
        let _ = timeout(Duration::from_secs(2), async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        tasks.shutdown().await;
    }

    async fn session(&self, mut connection: Connection) -> Result<(), Error> {
        let conn_id = Id::generate();
        let peer = connection.quic().remote_address().ip();
        let _close = CloseOnDrop(connection.quic().clone());
        let mut stream = timeout(OPEN_TIMEOUT, connection.accept_control())
            .await
            .map_err(|_| Error::Timeout)??;
        let envelope = timeout(OPEN_TIMEOUT, stream.recv())
            .await
            .map_err(|_| Error::Timeout)??;
        let request_id = envelope.request_id;
        let Some(Msg::Hello(hello)) = envelope.msg else {
            return reject(&connection, &mut stream, &hello_failure(request_id)).await;
        };
        if request_id == 0 {
            return reject(&connection, &mut stream, &hello_failure(request_id)).await;
        }
        if !ss_proto::is_supported_version(hello.protocol_version) {
            return reject(
                &connection,
                &mut stream,
                &ss_proto::unsupported_version(request_id),
            )
            .await;
        }
        let authed = match self.auth.hello(&hello, peer).await {
            Ok(authed) => authed,
            Err(error) => {
                tracing::debug!(%error, "hello rejected");
                let envelope =
                    v1::ServerEnvelope::error(request_id, error.code(), error.wire_message());
                return reject(&connection, &mut stream, &envelope).await;
            }
        };
        let mut subscription = self.bus.subscribe();
        let (outbound, outgoing) = mpsc::channel(registry::OUTBOUND_CAPACITY);
        let (kicks, kick_rx) = mpsc::channel(1);
        let quic = connection.quic().clone();
        let event = self.registry.register(Registration {
            conn_id,
            user_id: authed.user.id,
            connection: quic.clone(),
            outbound: outbound.clone(),
            kicks,
            client: ClientInfo {
                name: hello.client_name.clone(),
                version: hello.client_version.clone(),
                os: hello.os.clone(),
            },
        });
        let unregister = Unregister {
            registry: Arc::clone(&self.registry),
            bus: self.bus.clone(),
            conn_id,
        };
        if let Some(event) = event {
            self.bus.publish(Some(conn_id), event);
        }
        self.voice.attach(conn_id, authed.user.id, quic.clone());
        let voice_guard = VoiceGuard {
            voice: Arc::clone(&self.voice),
            bus: self.bus.clone(),
            conn_id,
        };
        let context = AuthedContext {
            user_id: authed.user.id,
            conn_id,
            perms_cache: authed.permissions,
        };
        let result = match self
            .prepare_welcome(&connection, &mut stream, &authed, request_id)
            .await
        {
            Ok(welcome) => {
                self.pump(
                    &context,
                    stream,
                    Pump {
                        outbound,
                        outgoing,
                        kicks: kick_rx,
                        quic,
                    },
                    &mut subscription,
                    request_id,
                    welcome,
                )
                .await
            }
            Err(error) => Err(error),
        };
        drop(subscription);
        drop(voice_guard);
        drop(unregister);
        result
    }

    async fn prepare_welcome(
        &self,
        connection: &Connection,
        stream: &mut ServerStream,
        authed: &auth::Authenticated,
        request_id: u32,
    ) -> Result<v1::ServerEnvelope, Error> {
        let mut snapshot = match snapshot::build_with(&self.perms, authed.user.id, |id| {
            self.registry.presence(id)
        })
        .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::debug!(%error, "snapshot failed");
                let envelope = v1::ServerEnvelope::error(
                    request_id,
                    v1::ErrorCode::Internal,
                    "internal error",
                );
                return reject(connection, stream, &envelope).await;
            }
        };
        self.voice.fill(&mut snapshot, authed.user.id);
        let welcome =
            welcome_envelope(request_id, authed, &self.auth.server_name().await, snapshot);
        if welcome.encoded_len() > ss_proto::MAX_FRAME_LEN {
            let error = v1::ServerEnvelope::error(
                request_id,
                v1::ErrorCode::Invalid,
                "welcome response is too large",
            );
            return reject(connection, stream, &error).await;
        }
        Ok(welcome)
    }

    async fn pump(
        &self,
        context: &AuthedContext,
        stream: ServerStream,
        pump: Pump,
        subscription: &mut broadcast::Receiver<ServerEvent>,
        request_id: u32,
        welcome: v1::ServerEnvelope,
    ) -> Result<(), Error> {
        let (sender, receiver) = stream.split();
        let media_conn = pump.quic.clone();
        let forwarder = Arc::clone(&self.media);
        let media_conn_id = context.conn_id;
        let write = write_outbound(
            sender,
            pump.outgoing,
            pump.kicks,
            pump.quic,
            Arc::clone(&self.registry),
        );
        let read = read_inbound(
            self,
            context,
            receiver,
            pump.outbound,
            subscription,
            request_id,
            welcome,
        );
        let datagrams = async move {
            loop {
                match media_conn.read_datagram().await {
                    Ok(bytes) => {
                        tokio::task::yield_now().await;
                        forwarder.forward(media_conn_id, &bytes);
                    }
                    Err(error) => return Err(Error::Connection(error)),
                }
            }
        };
        tokio::select! {
            result = read => result,
            result = write => result,
            result = datagrams => result,
        }
    }
}

struct Pump {
    outbound: mpsc::Sender<v1::ServerEnvelope>,
    outgoing: mpsc::Receiver<v1::ServerEnvelope>,
    kicks: mpsc::Receiver<Kick>,
    quic: quinn::Connection,
}

fn welcome_envelope(
    request_id: u32,
    authed: &auth::Authenticated,
    server_name: &str,
    snapshot: v1::Snapshot,
) -> v1::ServerEnvelope {
    v1::ServerEnvelope {
        request_id,
        msg: Some(server_envelope::Msg::Welcome(v1::Welcome {
            user: Some(wire_user(&authed.user)),
            server_info: Some(v1::ServerInfo {
                name: server_name.to_owned(),
                version: env!("CARGO_PKG_VERSION").into(),
                features: vec![
                    "auth".into(),
                    "roles".into(),
                    "moderation".into(),
                    "channels".into(),
                    "presence".into(),
                    "voice".into(),
                    "messages".into(),
                    "webhooks".into(),
                    "dms".into(),
                    "previews".into(),
                    "attachments".into(),
                ],
            }),
            resume_token: authed.resume_token.clone(),
            snapshot: Some(snapshot),
        })),
    }
}

fn wire_user(user: &crate::db::User) -> v1::User {
    let kind = match user.kind {
        ss_core::UserKind::Bot => v1::UserKind::Bot,
        _ => v1::UserKind::User,
    };
    v1::User {
        id: id_to_bytes(user.id),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        avatar_attachment_id: user.avatar_attachment_id.map(id_to_bytes),
        kind: kind.into(),
    }
}

async fn read_inbound(
    server: &Server,
    context: &AuthedContext,
    mut receiver: ss_net::control::ControlReceiver<v1::ClientEnvelope>,
    outbound: mpsc::Sender<v1::ServerEnvelope>,
    subscription: &mut broadcast::Receiver<ServerEvent>,
    mut last_request: u32,
    welcome: v1::ServerEnvelope,
) -> Result<(), Error> {
    outbound
        .send(welcome)
        .await
        .map_err(|_| Error::Control(ControlError::Closed))?;
    loop {
        tokio::select! {
            inbound = receiver.recv() => {
                let envelope = inbound?;
                if envelope.request_id <= last_request {
                    return Err(Error::Protocol);
                }
                last_request = envelope.request_id;
                if matches!(envelope.msg, Some(Msg::Ping(_))) {
                    server.registry.record_rtt(context.conn_id);
                }
                let answer = replies(server, context, &envelope).await;
                for frame in answer.frames {
                    outbound
                        .send(frame)
                        .await
                        .map_err(|_| Error::Control(ControlError::Closed))?;
                }
                for event in answer.events {
                    if let server_envelope::Msg::ReadStateUpdated(_) = &event {
                        for conn in server.registry.connection_ids(context.user_id) {
                            if conn == context.conn_id {
                                continue;
                            }
                            server.registry.enqueue(
                                conn,
                                v1::ServerEnvelope {
                                    request_id: 0,
                                    msg: Some(event.clone()),
                                },
                            );
                        }
                    } else {
                        server.bus.publish(Some(context.conn_id), event);
                    }
                }
            }
            event = subscription.recv() => {
                match event {
                    Ok(event) => deliver(server, context, &event),
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        server.registry.kick_slow(context.conn_id);
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}

fn deliver(server: &Server, context: &AuthedContext, event: &ServerEvent) {
    if event.origin == Some(context.conn_id) {
        return;
    }
    if !snapshot::delivers(&server.perms, context.user_id, &event.msg) {
        return;
    }
    server.registry.enqueue(
        context.conn_id,
        v1::ServerEnvelope {
            request_id: 0,
            msg: Some((*event.msg).clone()),
        },
    );
}

async fn write_outbound(
    mut sender: ss_net::control::ControlSender<v1::ServerEnvelope>,
    mut outgoing: mpsc::Receiver<v1::ServerEnvelope>,
    mut kick_rx: mpsc::Receiver<Kick>,
    quic: quinn::Connection,
    registry: Arc<ConnectionRegistry>,
) -> Result<(), Error> {
    loop {
        tokio::select! {
            biased;
            kick = kick_rx.recv() => {
                let Some(kick) = kick else { break };
                notify_and_close(&mut sender, &quic, &kick.notice, kick.code, kick.reason).await;
                break;
            }
            frame = outgoing.recv() => {
                let Some(frame) = frame else { break };
                match timeout(registry.slow_after(), sender.send(&frame)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => return Err(Error::Control(error)),
                    Err(_) => {
                        notify_and_close(
                            &mut sender,
                            &quic,
                            &registry::notice("control consumer too slow"),
                            registry::CLOSE_SLOW,
                            b"control consumer too slow",
                        )
                        .await;
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

async fn notify_and_close(
    sender: &mut ss_net::control::ControlSender<v1::ServerEnvelope>,
    quic: &quinn::Connection,
    notice: &v1::ServerEnvelope,
    code: u32,
    reason: &[u8],
) {
    let _ = timeout(NOTICE_TIMEOUT, sender.send(notice)).await;
    tokio::time::sleep(NOTICE_GRACE).await;
    quic.close(code.into(), reason);
}

async fn replies(
    server: &Server,
    context: &AuthedContext,
    envelope: &v1::ClientEnvelope,
) -> Answer {
    let request_id = envelope.request_id;
    tracing::trace!(
        user_id = %context.user_id,
        conn_id = %context.conn_id,
        permissions = context.perms_cache.bits(),
        "dispatch"
    );
    if let Some(Msg::Ping(ping)) = &envelope.msg {
        return Answer {
            frames: vec![v1::ServerEnvelope {
                request_id,
                msg: Some(server_envelope::Msg::Pong(v1::Pong {
                    sent_at: ping.sent_at,
                    server_time: Timestamp::now().as_millis(),
                })),
            }],
            events: Vec::new(),
        };
    }
    let Some(msg) = &envelope.msg else {
        return Answer {
            frames: vec![unsupported(request_id)],
            events: Vec::new(),
        };
    };
    if let Some(result) = server
        .voice
        .dispatch(context.conn_id, context.user_id, msg)
        .await
    {
        return voice_answer(request_id, result);
    }
    let result = if let Some(result) = server.presence.dispatch(context.user_id, msg) {
        result
    } else if let Some(result) = server.moderation.dispatch(context.user_id, msg).await {
        result
    } else if let Some(result) = server.roles.dispatch(context.user_id, msg).await {
        result
    } else if let Some(result) = server.dms.dispatch(context.user_id, msg).await {
        result
    } else if let Some(result) = server.channels.dispatch(context.user_id, msg).await {
        result
    } else if let Some(result) = server.messages.dispatch(context.user_id, msg).await {
        result
    } else if let Some(result) = server.webhooks.dispatch(context.user_id, msg).await {
        result
    } else if let Some(result) = server.profiles.dispatch(context.user_id, msg).await {
        result
    } else {
        return Answer {
            frames: vec![unsupported(request_id)],
            events: Vec::new(),
        };
    };
    match result {
        Ok(reply) => {
            server.voice.refresh_speak();
            let mut events = reply.events;
            let removed = deleted_channels(&events);
            for channel in removed {
                events.extend(server.voice.evacuate(channel));
            }
            let mut frames = vec![v1::ServerEnvelope::ack(request_id, reply.ack)];
            for event in &events {
                frames.push(v1::ServerEnvelope {
                    request_id: 0,
                    msg: Some(event.clone()),
                });
            }
            Answer { frames, events }
        }
        Err(error) => {
            tracing::debug!(%error, "command rejected");
            Answer {
                frames: vec![v1::ServerEnvelope::error(
                    request_id,
                    error.code(),
                    error.wire_message(),
                )],
                events: Vec::new(),
            }
        }
    }
}

fn voice_answer(request_id: u32, result: Result<voice::Outcome, crate::handlers::Error>) -> Answer {
    match result {
        Ok(outcome) => {
            let mut frames = vec![v1::ServerEnvelope::ack(request_id, outcome.ack)];
            for event in outcome.direct.iter().chain(&outcome.broadcast) {
                frames.push(v1::ServerEnvelope {
                    request_id: 0,
                    msg: Some(event.clone()),
                });
            }
            Answer {
                frames,
                events: outcome.broadcast,
            }
        }
        Err(error) => {
            tracing::debug!(%error, "voice command rejected");
            Answer {
                frames: vec![v1::ServerEnvelope::error(
                    request_id,
                    error.code(),
                    error.wire_message(),
                )],
                events: Vec::new(),
            }
        }
    }
}

fn deleted_channels(events: &[server_envelope::Msg]) -> Vec<Id> {
    events
        .iter()
        .filter_map(|event| {
            let server_envelope::Msg::ChannelDeleted(deleted) = event else {
                return None;
            };
            let channel = deleted.channel.as_ref()?;
            id_from_bytes(&channel.id).ok()
        })
        .collect()
}

fn unsupported(request_id: u32) -> v1::ServerEnvelope {
    v1::ServerEnvelope::error(request_id, v1::ErrorCode::Invalid, "unsupported command")
}

fn hello_failure(request_id: u32) -> v1::ServerEnvelope {
    v1::ServerEnvelope::error(
        request_id,
        v1::ErrorCode::Invalid,
        "first request must be Hello with a nonzero id",
    )
}

async fn reject<T>(
    connection: &Connection,
    stream: &mut ServerStream,
    envelope: &v1::ServerEnvelope,
) -> Result<T, Error> {
    stream.send(envelope).await?;
    // Leave the connection open long enough for the error frame to arrive.
    // `CloseOnDrop` closes it when this session returns.
    let _ = timeout(Duration::from_secs(1), connection.quic().closed()).await;
    Err(Error::Protocol)
}

struct VoiceGuard {
    voice: Arc<Voice>,
    bus: EventBus,
    conn_id: Id,
}

impl Drop for VoiceGuard {
    fn drop(&mut self) {
        for event in self.voice.detach(self.conn_id) {
            self.bus.publish(None, event);
        }
    }
}

struct Unregister {
    registry: Arc<ConnectionRegistry>,
    bus: EventBus,
    conn_id: Id,
}

impl Drop for Unregister {
    fn drop(&mut self) {
        if let Some(event) = self.registry.unregister(self.conn_id) {
            self.bus.publish(None, event);
        }
    }
}

struct CloseOnDrop(quinn::Connection);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if self.0.close_reason().is_none() {
            self.0.close(0_u32.into(), b"session ended");
        }
    }
}
