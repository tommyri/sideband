//! Authenticated control-stream owner.
//!
//! The voice spike in the crate root still accepts any username. This server
//! requires [`auth`] and then answers `Ping`, presence, profile, role, channel,
//! direct message, text, and voice commands. Events are delivered to the actor on the command reply and
//! published for every other visible session. Media datagrams are forwarded
//! only to members of the same voice channel.
//!
//! Each command runs on its own task while the session keeps draining the
//! event bus, so a command that waits on the database does not make the
//! session fall behind. Commands from one connection still run one at a
//! time, in order. DM channel events go only to that DM's members and are
//! applied from the event itself; role, override and guild channel changes
//! make each session diff its guild channel view, at most once per
//! [`REFRESH_DEBOUNCE`] for a burst.

use crate::{
    Error, MAX_CONNECTIONS,
    auth::{self, Auth},
    events::{EventBus, ServerEvent},
    handlers::{
        Channels, DirectMessages, Messages, Moderation, Presence, Profiles, Roles, Webhooks,
    },
    limits::{self, KeyedLimiter, PeerSessions, quota},
    media::{self, ForwardOutcome, Forwarder},
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
use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinSet,
    time::{Instant, timeout},
};

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const NOTICE_TIMEOUT: Duration = Duration::from_secs(1);
/// Lets the notice frame leave the socket before `CONNECTION_CLOSE`.
const NOTICE_GRACE: Duration = Duration::from_millis(100);
/// How long a session waits after a role, override or guild channel change
/// before it diffs its channel view, so a burst of changes costs one diff.
/// An event that names a channel flushes a pending diff first.
pub const REFRESH_DEBOUNCE: Duration = Duration::from_millis(100);

/// Identity and permission cache for one accepted control stream.
#[derive(Clone)]
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
    hellos: KeyedLimiter<IpAddr>,
    commands: KeyedLimiter<Id>,
    peers: PeerSessions,
    /// Full channel-view recomputations, for tests and diagnostics.
    channel_views: std::sync::atomic::AtomicU64,
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
        let (voice, media) = Voice::open(Arc::clone(&perms), Arc::clone(&registry));
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
            hellos: KeyedLimiter::new(quota(limits::HELLO_BURST, limits::HELLO_PERIOD)),
            commands: KeyedLimiter::new(quota(limits::COMMAND_BURST, limits::COMMAND_PERIOD)),
            peers: PeerSessions::default(),
            channel_views: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Caps concurrent sessions, pending handshakes included, per address.
    /// The default is [`limits::SESSIONS_PER_IP`]. Live sessions stay open.
    pub fn limit_sessions_per_ip(&self, max: usize) {
        self.peers.set_max(max);
    }

    /// Opaque-media counters for the authenticated router.
    pub fn media_stats(&self) -> media::Stats {
        self.media.stats()
    }

    /// Video publication and relay counters.
    pub fn stream_stats(&self) -> media::streams::Stats {
        self.voice.streams().stats()
    }

    /// How many times a session has recomputed its whole channel view since
    /// start. Each one reads every live channel.
    #[must_use]
    pub fn channel_view_count(&self) -> u64 {
        self.channel_views
            .load(std::sync::atomic::Ordering::Relaxed)
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
            .merge(crate::http::moderation_admin::router(
                Arc::clone(&auth),
                Arc::clone(&perms),
            ))
            .merge(crate::http::search::router(
                Arc::clone(&auth),
                Arc::clone(&perms),
            ))
            .merge(crate::http::audit::router(
                Arc::clone(&auth),
                Arc::clone(&perms),
            ))
            .merge(crate::http::invites::router(
                Arc::clone(&auth),
                Arc::clone(&perms),
            ))
            .merge(crate::http::server_settings::router(
                Arc::clone(&auth),
                Arc::clone(&perms),
                self.files.clone(),
            ))
            .merge(crate::http::role_admin::router(
                Arc::clone(&auth),
                Arc::clone(&perms),
            ))
            .merge(crate::http::webhook_admin::router(
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
                    // A spoofed source address must not take a session or
                    // per-address slot. A stateless Retry costs the server
                    // nothing; only a peer that echoes it gets counted.
                    if !incoming.remote_address_validated() {
                        incoming.retry_or_refuse();
                        continue;
                    }
                    if tasks.len() >= MAX_CONNECTIONS {
                        incoming.refuse();
                        continue;
                    }
                    let Some(slot) = self.peers.acquire(incoming.remote_address().ip()) else {
                        incoming.refuse();
                        continue;
                    };
                    let server = Arc::clone(&self);
                    tasks.spawn(async move {
                        let _slot = slot;
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

    async fn session(self: &Arc<Self>, mut connection: Connection) -> Result<(), Error> {
        let mut conn_id = Id::generate();
        let peer = connection.quic().remote_address().ip();
        let _close = CloseOnDrop(connection.quic().clone());
        let mut stream = timeout(OPEN_TIMEOUT, connection.accept_control())
            .await
            .map_err(|_| Error::Timeout)??;
        let envelope = timeout(OPEN_TIMEOUT, stream.recv())
            .await
            .map_err(|_| Error::Timeout)??;
        let request_id = envelope.request_id;
        let Some(Msg::Hello(mut hello)) = envelope.msg else {
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
        let authed = match self.authenticate(&mut hello, peer, request_id).await {
            Ok(authed) => authed,
            Err(envelope) => return reject(&connection, &mut stream, &envelope).await,
        };
        crate::observe::record_session();
        let mut subscription = self.bus.subscribe();
        let (outbound, outgoing) = mpsc::channel(registry::OUTBOUND_CAPACITY);
        let (direct, direct_rx) = mpsc::channel(registry::OUTBOUND_CAPACITY);
        let (kicks, kick_rx) = mpsc::channel(1);
        let quic = connection.quic().clone();
        conn_id = self.attach_voice(conn_id, &authed, &quic);
        let event = self.registry.register(Registration {
            conn_id,
            user_id: authed.user.id,
            connection: quic.clone(),
            outbound: outbound.clone(),
            direct,
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
        let mut voice_guard = VoiceGuard {
            voice: Arc::clone(&self.voice),
            bus: self.bus.clone(),
            conn_id,
            user_id: authed.user.id,
            resume_token: authed.resume_token.clone(),
            reconnect: false,
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
                        direct: direct_rx,
                        kicks: kick_rx,
                        quic: quic.clone(),
                    },
                    &mut subscription,
                    request_id,
                    welcome,
                )
                .await
            }
            Err(error) => Err(error),
        };
        voice_guard.reconnect = reconnectable(close_reason(&quic).await);
        drop(subscription);
        drop(unregister);
        drop(voice_guard);
        result
    }

    /// Registers the connection with voice, resuming a leased seat when the
    /// member may still hold it. A dropped lease's leave is published.
    fn attach_voice(&self, conn: Id, authed: &auth::Authenticated, quic: &quinn::Connection) -> Id {
        let (conn, dropped) =
            self.voice
                .connect(conn, &authed.resume_token, authed.user.id, quic.clone());
        for event in dropped {
            self.bus.publish(None, event);
        }
        conn
    }

    /// Spends the per-address Hello quota, checks the credentials, and wipes
    /// them from `hello`. `Err` is the frame to send before closing.
    async fn authenticate(
        &self,
        hello: &mut v1::Hello,
        peer: IpAddr,
        request_id: u32,
    ) -> Result<auth::Authenticated, Box<v1::ServerEnvelope>> {
        if let Err(wait) = self.hellos.check(&limits::peer_key(peer)) {
            scrub(hello);
            return Err(Box::new(v1::ServerEnvelope::rate_limited(
                request_id,
                "too many connection attempts",
                wait,
            )));
        }
        let outcome = self.auth.hello(hello, peer).await;
        scrub(hello);
        outcome.map_err(|error| {
            tracing::debug!(%error, "hello rejected");
            Box::new(error_envelope(
                request_id,
                error.code(),
                error.wire_message(),
                error.retry_after(),
            ))
        })
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
        let mut welcome =
            welcome_envelope(request_id, authed, &self.auth.server_name().await, snapshot);
        // Never refuse a login over its DMs: halve them until the frame fits.
        // The client loads the rest with `ListDms`.
        while welcome.encoded_len() > ss_proto::MAX_FRAME_LEN {
            let Some(server_envelope::Msg::Welcome(v1::Welcome {
                snapshot: Some(snapshot),
                ..
            })) = welcome.msg.as_mut()
            else {
                break;
            };
            if snapshot.direct_messages.is_empty() {
                break;
            }
            let keep = snapshot.direct_messages.len() / 2;
            snapshot::keep_direct(snapshot, keep);
        }
        if welcome.encoded_len() > ss_proto::MAX_FRAME_LEN {
            tracing::warn!(user_id = %authed.user.id, "welcome exceeds one frame without DMs");
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
        self: &Arc<Self>,
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
        let media_registry = Arc::clone(&self.registry);
        let media_conn_id = context.conn_id;
        let video_conn = pump.quic.clone();
        let video = self.voice.streams();
        let write = write_outbound(
            sender,
            pump.outgoing,
            pump.direct,
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
                        if let ForwardOutcome::Disconnect(wait) =
                            forwarder.forward(media_conn_id, &bytes)
                        {
                            media_registry.enqueue(
                                media_conn_id,
                                v1::ServerEnvelope::rate_limited(0, "media bitrate exceeded", wait),
                            );
                            tokio::time::sleep(NOTICE_GRACE).await;
                            media_conn.close(
                                registry::CLOSE_RATE_LIMITED.into(),
                                b"media bitrate exceeded",
                            );
                            return Err(Error::Protocol);
                        }
                    }
                    Err(error) => return Err(Error::Connection(error)),
                }
            }
        };
        tokio::select! {
            result = read => result,
            result = write => result,
            result = datagrams => result,
            result = video.run(media_conn_id, video_conn) => Err(Error::Connection(result)),
        }
    }
}

struct Pump {
    outbound: mpsc::Sender<v1::ServerEnvelope>,
    outgoing: mpsc::Receiver<v1::ServerEnvelope>,
    direct: mpsc::Receiver<v1::ServerEnvelope>,
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
                    "role-management-v2".into(),
                    "moderation".into(),
                    "channels".into(),
                    "presence".into(),
                    "voice".into(),
                    "video-streams".into(),
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

/// What this session has told its client about channels.
struct View {
    /// Guild channels, as last sent. Diffed by [`sync_channels`].
    guild: BTreeMap<Id, v1::Channel>,
    /// DMs the client was sent, by `Welcome`, `ListDms` or an event.
    direct: HashSet<Id>,
    /// When a debounced guild diff is due.
    refresh_at: Option<Instant>,
}

impl View {
    fn from_welcome(welcome: &v1::ServerEnvelope) -> Self {
        let mut view = Self {
            guild: BTreeMap::new(),
            direct: HashSet::new(),
            refresh_at: None,
        };
        if let Some(server_envelope::Msg::Welcome(value)) = &welcome.msg
            && let Some(snapshot) = &value.snapshot
        {
            for channel in &snapshot.channels {
                if let Ok(id) = id_from_bytes(&channel.id) {
                    view.guild.insert(id, channel.clone());
                }
            }
            view.direct.extend(
                snapshot
                    .direct_messages
                    .iter()
                    .filter_map(|channel| id_from_bytes(&channel.id).ok()),
            );
        }
        view
    }
}

/// The command running for this connection. Dropping it aborts the task, as
/// dropping the connection always cancelled its command.
struct Inflight {
    request_id: u32,
    task: tokio::task::JoinHandle<Answer>,
}

impl Inflight {
    async fn answer(&mut self) -> Answer {
        match (&mut self.task).await {
            Ok(answer) => answer,
            Err(error) => {
                tracing::warn!(%error, "command task failed");
                Answer {
                    frames: vec![v1::ServerEnvelope::error(
                        self.request_id,
                        v1::ErrorCode::Internal,
                        "internal error",
                    )],
                    events: Vec::new(),
                }
            }
        }
    }
}

impl Drop for Inflight {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn answered(inflight: &mut Option<Inflight>) -> Answer {
    match inflight {
        Some(inflight) => inflight.answer().await,
        None => std::future::pending().await,
    }
}

async fn due(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn send(
    outbound: &mpsc::Sender<v1::ServerEnvelope>,
    frame: v1::ServerEnvelope,
) -> Result<(), Error> {
    outbound
        .send(frame)
        .await
        .map_err(|_| Error::Control(ControlError::Closed))
}

fn event_frame(msg: server_envelope::Msg) -> v1::ServerEnvelope {
    v1::ServerEnvelope {
        request_id: 0,
        msg: Some(msg),
    }
}

async fn read_inbound(
    server: &Arc<Server>,
    context: &AuthedContext,
    mut receiver: ss_net::control::ControlReceiver<v1::ClientEnvelope>,
    outbound: mpsc::Sender<v1::ServerEnvelope>,
    subscription: &mut broadcast::Receiver<ServerEvent>,
    mut last_request: u32,
    welcome: v1::ServerEnvelope,
) -> Result<(), Error> {
    let mut view = View::from_welcome(&welcome);
    if let Some(server_envelope::Msg::Welcome(value)) = &welcome.msg
        && let Some(snapshot) = &value.snapshot
    {
        server.registry.seed_voice(
            context.conn_id,
            snapshot
                .voice_states
                .iter()
                .filter_map(|state| id_from_bytes(&state.conn_id).ok()),
        );
    }
    send(&outbound, welcome).await?;
    let mut inflight: Option<Inflight> = None;
    loop {
        tokio::select! {
            answer = answered(&mut inflight) => {
                inflight = None;
                finish(server, context, &mut view, &outbound, answer).await?;
            }
            inbound = receiver.recv(), if inflight.is_none() => {
                let envelope = inbound?;
                if envelope.request_id <= last_request {
                    return Err(Error::Protocol);
                }
                last_request = envelope.request_id;
                if let Some(Msg::Ping(ping)) = &envelope.msg {
                    // Ping is exempt, and costs nothing, so keepalive never fails.
                    server.registry.record_rtt(context.conn_id);
                    send(&outbound, pong(envelope.request_id, *ping)).await?;
                    continue;
                }
                if let Err(wait) = server.commands.check(&context.user_id) {
                    let frame = v1::ServerEnvelope::rate_limited(
                        envelope.request_id,
                        "too many commands",
                        wait,
                    );
                    send(&outbound, frame).await?;
                    continue;
                }
                let task_server = Arc::clone(server);
                let task_context = context.clone();
                inflight = Some(Inflight {
                    request_id: envelope.request_id,
                    task: tokio::spawn(async move {
                        replies(&task_server, &task_context, &envelope).await
                    }),
                });
            }
            event = subscription.recv() => {
                match event {
                    Ok(event) => on_event(server, context, &mut view, &outbound, &event).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // The writer sends the notice and closes; the session
                        // ends with it. Nothing more is read or delivered.
                        server.registry.kick_slow(context.conn_id);
                        std::future::pending::<()>().await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            () = due(view.refresh_at) => {
                sync_channels(server, context, &mut view, &outbound).await?;
            }
        }
    }
    Ok(())
}

/// Sends a finished command's reply, updates this session's view, and
/// publishes the events for everyone else.
async fn finish(
    server: &Server,
    context: &AuthedContext,
    view: &mut View,
    outbound: &mpsc::Sender<v1::ServerEnvelope>,
    answer: Answer,
) -> Result<(), Error> {
    let refresh = answer.events.iter().any(refreshes_guild);
    for frame in answer.frames {
        if frame.msg.as_ref().is_some_and(channel_event) {
            continue;
        }
        if !reply_visible(server, context, &frame) {
            continue;
        }
        if let Some(server_envelope::Msg::Ack(v1::Ack {
            result: Some(v1::ack::Result::DmPage(page)),
            ..
        })) = &frame.msg
        {
            view.direct.extend(
                page.channels
                    .iter()
                    .filter_map(|channel| id_from_bytes(&channel.id).ok()),
            );
        }
        send(outbound, frame).await?;
    }
    for event in &answer.events {
        if direct_channel(event).is_some() {
            apply_direct(server, context, view, outbound, event).await?;
        }
    }
    if refresh {
        sync_channels(server, context, view, outbound).await?;
    }
    publish(server, context, answer.events);
    Ok(())
}

/// Publishes a command's events. Other sessions only diff their view on a
/// guild channel event and never forward it, so one stands for the whole
/// batch: deleting a category of hundreds of channels must not overflow the
/// bus and close every other session.
fn publish(server: &Server, context: &AuthedContext, events: Vec<server_envelope::Msg>) {
    let mut guild_published = false;
    for event in events {
        if let server_envelope::Msg::ReadStateUpdated(_) = &event {
            for conn in server.registry.connection_ids(context.user_id) {
                if conn == context.conn_id {
                    continue;
                }
                server.registry.enqueue(conn, event_frame(event.clone()));
            }
            continue;
        }
        if channel_event(&event) && direct_channel(&event).is_none() {
            if guild_published {
                continue;
            }
            guild_published = true;
        }
        server.bus.publish(Some(context.conn_id), event);
    }
}

/// Handles one bus event for this session.
async fn on_event(
    server: &Server,
    context: &AuthedContext,
    view: &mut View,
    outbound: &mpsc::Sender<v1::ServerEnvelope>,
    event: &ServerEvent,
) -> Result<(), Error> {
    if event.origin == Some(context.conn_id) {
        return Ok(());
    }
    let msg = &*event.msg;
    if direct_channel(msg).is_some() {
        return apply_direct(server, context, view, outbound, msg).await;
    }
    if refreshes_guild(msg) {
        view.refresh_at
            .get_or_insert_with(|| Instant::now() + REFRESH_DEBOUNCE);
    }
    if channel_event(msg) {
        return Ok(());
    }
    if let Some(channel) = snapshot::event_channel(msg) {
        // A channel that just became visible is announced before its events.
        if view.refresh_at.is_some() {
            sync_channels(server, context, view, outbound).await?;
        }
        announce_direct(server, context, view, outbound, channel).await?;
    }
    deliver(server, context, outbound, msg).await
}

/// Sends a DM the client was never told about (it was past the `Welcome`
/// page) before the first event in it.
async fn announce_direct(
    server: &Server,
    context: &AuthedContext,
    view: &mut View,
    outbound: &mpsc::Sender<v1::ServerEnvelope>,
    channel: Id,
) -> Result<(), Error> {
    if view.direct.contains(&channel)
        || !server
            .perms
            .direct_members(channel)
            .is_some_and(|members| members.contains(&context.user_id))
    {
        return Ok(());
    }
    let wire = snapshot::direct_channel(&server.perms, context.user_id, channel)
        .await
        .map_err(|error| {
            tracing::debug!(%error, "dm lookup failed");
            Error::Protocol
        })?;
    let Some(wire) = wire else {
        return Ok(());
    };
    view.direct.insert(channel);
    send(
        outbound,
        event_frame(server_envelope::Msg::ChannelCreated(v1::ChannelCreated {
            channel: Some(wire),
        })),
    )
    .await
}

/// Applies a DM channel event from its own payload: members see it created
/// or updated, a session that knew the DM and is no longer in it sees it
/// deleted, and everyone else sees nothing. No database read.
async fn apply_direct(
    server: &Server,
    context: &AuthedContext,
    view: &mut View,
    outbound: &mpsc::Sender<v1::ServerEnvelope>,
    msg: &server_envelope::Msg,
) -> Result<(), Error> {
    let Some(channel) = direct_channel(msg) else {
        return Ok(());
    };
    let Ok(id) = id_from_bytes(&channel.id) else {
        return Ok(());
    };
    let deleted = matches!(msg, server_envelope::Msg::ChannelDeleted(_));
    let me = id_to_bytes(context.user_id);
    let member = !deleted && channel.member_ids.contains(&me);
    let known = view.direct.contains(&id);
    let out = if member {
        let mut wire = channel.clone();
        wire.effective_permissions = server
            .perms
            .channel_permissions(context.user_id, id)
            .ok()
            .map(|permissions| permissions.bits());
        view.direct.insert(id);
        if known {
            server_envelope::Msg::ChannelUpdated(v1::ChannelUpdated {
                channel: Some(wire),
            })
        } else {
            server_envelope::Msg::ChannelCreated(v1::ChannelCreated {
                channel: Some(wire),
            })
        }
    } else if known {
        view.direct.remove(&id);
        server_envelope::Msg::ChannelDeleted(v1::ChannelDeleted {
            channel: Some(channel.clone()),
        })
    } else {
        return Ok(());
    };
    send(outbound, event_frame(out)).await
}

fn pong(request_id: u32, ping: v1::Ping) -> v1::ServerEnvelope {
    v1::ServerEnvelope {
        request_id,
        msg: Some(server_envelope::Msg::Pong(v1::Pong {
            sent_at: ping.sent_at,
            server_time: Timestamp::now().as_millis(),
        })),
    }
}

fn channel_event(event: &server_envelope::Msg) -> bool {
    matches!(
        event,
        server_envelope::Msg::ChannelCreated(_)
            | server_envelope::Msg::ChannelUpdated(_)
            | server_envelope::Msg::ChannelDeleted(_)
    )
}

/// The channel of a DM or group-DM channel event.
fn direct_channel(event: &server_envelope::Msg) -> Option<&v1::Channel> {
    let channel = match event {
        server_envelope::Msg::ChannelCreated(v1::ChannelCreated { channel })
        | server_envelope::Msg::ChannelUpdated(v1::ChannelUpdated { channel })
        | server_envelope::Msg::ChannelDeleted(v1::ChannelDeleted { channel }) => {
            channel.as_ref()?
        }
        _ => return None,
    };
    matches!(
        v1::ChannelKind::try_from(channel.kind),
        Ok(v1::ChannelKind::Dm | v1::ChannelKind::GroupDm)
    )
    .then_some(channel)
}

/// Whether `event` can change which guild channels a session sees.
fn refreshes_guild(event: &server_envelope::Msg) -> bool {
    (channel_event(event) && direct_channel(event).is_none())
        || matches!(
            event,
            server_envelope::Msg::RoleCreated(_)
                | server_envelope::Msg::RoleUpdated(_)
                | server_envelope::Msg::RoleDeleted(_)
                | server_envelope::Msg::MemberRolesUpdated(_)
        )
}

async fn sync_channels(
    server: &Server,
    context: &AuthedContext,
    view: &mut View,
    outbound: &mpsc::Sender<v1::ServerEnvelope>,
) -> Result<(), Error> {
    view.refresh_at = None;
    server
        .channel_views
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let current = snapshot::channel_view(&server.perms, context.user_id)
        .await
        .map_err(|error| {
            tracing::debug!(%error, "channel view failed");
            Error::Protocol
        })?;
    for (id, old) in &view.guild {
        if !current.contains_key(id) {
            send(
                outbound,
                event_frame(server_envelope::Msg::ChannelDeleted(v1::ChannelDeleted {
                    channel: Some(old.clone()),
                })),
            )
            .await?;
        }
    }
    for (id, channel) in &current {
        let msg = match view.guild.get(id) {
            None => server_envelope::Msg::ChannelCreated(v1::ChannelCreated {
                channel: Some(channel.clone()),
            }),
            Some(old) if old != channel => {
                server_envelope::Msg::ChannelUpdated(v1::ChannelUpdated {
                    channel: Some(channel.clone()),
                })
            }
            _ => continue,
        };
        send(outbound, event_frame(msg)).await?;
    }
    view.guild = current;
    Ok(())
}

/// Whether an event frame on the actor's own reply may be sent. The ack and
/// error frames always go. Events pass the same checks as fan-out, so a
/// moderator's reply never names a hidden channel, except that the actor
/// always sees events about invites they created.
fn reply_visible(server: &Server, context: &AuthedContext, frame: &v1::ServerEnvelope) -> bool {
    if frame.request_id != 0 {
        return true;
    }
    let Some(msg) = &frame.msg else {
        return true;
    };
    let own_invite = match msg {
        server_envelope::Msg::InviteCreated(v1::InviteCreated { invite })
        | server_envelope::Msg::InviteRevoked(v1::InviteRevoked { invite }) => invite
            .as_ref()
            .is_some_and(|invite| invite.created_by == id_to_bytes(context.user_id)),
        _ => false,
    };
    (own_invite || snapshot::delivers(&server.perms, context.user_id, msg))
        && server.registry.admit_voice(context.conn_id, frame)
}

/// Sends a bus event to this session when it passes the visibility filter
/// and the voice-state gate. Waits for room in the session's queue: a client
/// that reads too slowly falls behind on the bus and is closed, never sent
/// a stream with a frame missing.
async fn deliver(
    server: &Server,
    context: &AuthedContext,
    outbound: &mpsc::Sender<v1::ServerEnvelope>,
    msg: &server_envelope::Msg,
) -> Result<(), Error> {
    if !snapshot::delivers(&server.perms, context.user_id, msg) {
        return Ok(());
    }
    let frame = event_frame(msg.clone());
    if !server.registry.admit_voice(context.conn_id, &frame) {
        return Ok(());
    }
    send(outbound, frame).await
}

/// Writes the session's own queue and frames addressed to it by other tasks,
/// and carries out a kick. A write that stalls past the slow-consumer grace
/// closes the connection; a kick that arrives during a stalled write closes
/// it without the notice, since the stream is mid-frame.
async fn write_outbound(
    mut sender: ss_net::control::ControlSender<v1::ServerEnvelope>,
    mut outgoing: mpsc::Receiver<v1::ServerEnvelope>,
    mut direct: mpsc::Receiver<v1::ServerEnvelope>,
    mut kick_rx: mpsc::Receiver<Kick>,
    quic: quinn::Connection,
    registry: Arc<ConnectionRegistry>,
) -> Result<(), Error> {
    loop {
        let frame = tokio::select! {
            biased;
            kick = kick_rx.recv() => {
                let Some(kick) = kick else { break };
                notify_and_close(&mut sender, &quic, &kick.notice, kick.code, kick.reason).await;
                break;
            }
            frame = direct.recv() => frame,
            frame = outgoing.recv() => frame,
        };
        let Some(frame) = frame else { break };
        let outcome = {
            let write = timeout(registry.slow_after(), sender.send(&frame));
            tokio::pin!(write);
            tokio::select! {
                biased;
                written = &mut write => Written::Done(written),
                kick = kick_rx.recv() => {
                    let finished = timeout(NOTICE_TIMEOUT, &mut write).await;
                    Written::Kicked(kick.map(Box::new), matches!(finished, Ok(Ok(Ok(())))))
                }
            }
        };
        match outcome {
            Written::Done(Ok(Ok(()))) => {}
            Written::Done(Ok(Err(error))) => return Err(Error::Control(error)),
            Written::Done(Err(_)) => {
                crate::observe::record_slow_disconnect();
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
            Written::Kicked(Some(kick), true) => {
                notify_and_close(&mut sender, &quic, &kick.notice, kick.code, kick.reason).await;
                break;
            }
            Written::Kicked(kick, _) => {
                if let Some(kick) = kick {
                    quic.close(kick.code.into(), kick.reason);
                }
                break;
            }
        }
    }
    Ok(())
}

enum Written {
    Done(Result<Result<(), ControlError>, tokio::time::error::Elapsed>),
    /// A kick arrived mid-write; `true` when the write then finished.
    Kicked(Option<Box<Kick>>, bool),
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
            frames: vec![pong(request_id, *ping)],
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
            let mut events = reply.events;
            events.extend(server.voice.refresh_seats());
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
                frames: vec![error_envelope(
                    request_id,
                    error.code(),
                    error.wire_message(),
                    error.retry_after(),
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
                frames: vec![error_envelope(
                    request_id,
                    error.code(),
                    error.wire_message(),
                    error.retry_after(),
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

/// `Error` for `code`, carrying `retry_after_ms` when the failure is a quota.
fn error_envelope(
    request_id: u32,
    code: v1::ErrorCode,
    message: String,
    retry_after: Option<Duration>,
) -> v1::ServerEnvelope {
    match retry_after {
        Some(wait) => v1::ServerEnvelope::rate_limited(request_id, message, wait),
        None => v1::ServerEnvelope::error(request_id, code, message),
    }
}

/// Overwrites the credentials in a `Hello` once they have been checked, so
/// the password and token bytes do not linger in freed memory.
fn scrub(hello: &mut v1::Hello) {
    use zeroize::Zeroize;
    match hello.auth.as_mut() {
        Some(v1::hello::Auth::Login(login)) => login.password.zeroize(),
        Some(v1::hello::Auth::Register(register)) => {
            register.password.zeroize();
            register.invite_code.zeroize();
        }
        Some(v1::hello::Auth::SessionToken(token) | v1::hello::Auth::ResumeToken(token)) => {
            token.zeroize();
        }
        None => {}
    }
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
    user_id: Id,
    resume_token: String,
    reconnect: bool,
}

impl Drop for VoiceGuard {
    fn drop(&mut self) {
        if self.reconnect
            && self.voice.lease(
                self.resume_token.clone(),
                self.conn_id,
                self.user_id,
                self.bus.clone(),
            )
        {
            return;
        }
        for event in self.voice.detach(self.conn_id) {
            self.bus.publish(None, event);
        }
    }
}

fn reconnectable(reason: Option<quinn::ConnectionError>) -> bool {
    match reason {
        Some(quinn::ConnectionError::ApplicationClosed(close)) => close.error_code != 0_u32.into(),
        Some(quinn::ConnectionError::Reset | quinn::ConnectionError::TimedOut) => true,
        _ => false,
    }
}

async fn close_reason(connection: &quinn::Connection) -> Option<quinn::ConnectionError> {
    match connection.close_reason() {
        Some(reason) => Some(reason),
        None => timeout(Duration::from_secs(1), connection.closed())
            .await
            .ok(),
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
