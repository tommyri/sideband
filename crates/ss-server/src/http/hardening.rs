//! Headers, per-address limits, deadlines, and body caps for public HTTP.
//!
//! [`harden`] wraps every public route. [`serve`] owns the accept loop: it
//! caps open connections overall and per address, drops a client that has
//! not finished its request head in [`HEADER_TIMEOUT`], closes connections
//! idle for [`IDLE_TIMEOUT`] or older than [`MAX_AGE`] (HTTP/1 keep-alive and
//! HTTP/2 alike), and records the peer address the request quota keys on.

use std::{
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto,
    service::TowerToHyperService,
};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpListener,
    sync::Semaphore,
};
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

use crate::limits::{HTTP_BURST, HTTP_PERIOD, KeyedLimiter, PeerSessions, quota};

/// Deadline for a whole request, body included, on ordinary routes.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Deadline for an attachment upload or download.
pub const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);
/// Time a new connection gets to send its request line and headers.
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// Body cap for every route that does not set its own (uploads do).
pub const MAX_BODY: usize = 64 * 1024;
/// Open TCP connections on one public listener. Further accepts wait.
pub const MAX_CONNECTIONS: usize = 1024;
/// Open TCP connections from one address (IPv4, or IPv6 /64). Further
/// connections from it are closed on accept.
pub const CONNECTIONS_PER_ADDRESS: usize = 64;
/// A connection with no bytes in either direction for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// A connection is asked to close at this age, whatever it is doing. A
/// request in flight then has [`TRANSFER_TIMEOUT`] to finish.
pub const MAX_AGE: Duration = Duration::from_mins(10);

/// Connection limits for [`serve_with`]. `Default` is the constants above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeLimits {
    /// Time to send a request head ([`HEADER_TIMEOUT`]).
    pub header_timeout: Duration,
    /// Silence before an idle close ([`IDLE_TIMEOUT`]).
    pub idle_timeout: Duration,
    /// Age at which a connection is closed ([`MAX_AGE`]).
    pub max_age: Duration,
    /// Open connections per address ([`CONNECTIONS_PER_ADDRESS`]).
    pub per_address: usize,
}

impl Default for ServeLimits {
    fn default() -> Self {
        Self {
            header_timeout: HEADER_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
            max_age: MAX_AGE,
            per_address: CONNECTIONS_PER_ADDRESS,
        }
    }
}

/// Content-Security-Policy on every response. API bodies are data, never a
/// document: nothing loads, nothing frames it, and an attachment opened as a
/// top-level page runs sandboxed.
pub const CSP: &str = "default-src 'none'; frame-ancestors 'none'; sandbox";

const ATTACHMENTS: &str = "/api/v1/attachments";

/// Adds strict headers, the per-address quota, deadlines, and the default
/// body cap to `router`. Routes that set their own `DefaultBodyLimit` keep it.
pub fn harden(router: Router) -> Router {
    let limiter = Arc::new(KeyedLimiter::new(quota(HTTP_BURST, HTTP_PERIOD)));
    router
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn(deadline))
        .layer(middleware::from_fn_with_state(limiter, per_address))
        .layer(middleware::from_fn(headers))
}

/// A 429 with `Retry-After` in whole seconds, rounded up.
#[must_use]
pub fn too_many_requests(retry_after: Duration) -> Response {
    let seconds = retry_after.as_millis().div_ceil(1000).max(1);
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "error": "rate limited" })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// Whether `headers` declare `wanted` as the media type, ignoring parameters
/// and case.
#[must_use]
pub fn has_content_type(headers: &HeaderMap, wanted: &str) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case(wanted))
}

/// Serves `app` on `listener` until `stop` resolves, with the default
/// [`ServeLimits`]. The peer address is available to handlers as
/// `ConnectInfo<SocketAddr>`.
pub async fn serve(listener: TcpListener, app: Router, stop: impl Future<Output = ()>) {
    serve_with_acceptor(listener, app, stop, ServeLimits::default(), None).await;
}

/// HTTPS variant of [`serve`]. TLS handshakes use the supplied live rustls
/// resolver and are subject to the same connection and header deadlines.
pub async fn serve_tls(
    listener: TcpListener,
    app: Router,
    config: Arc<rustls::ServerConfig>,
    stop: impl Future<Output = ()>,
) {
    serve_with_acceptor(
        listener,
        app,
        stop,
        ServeLimits::default(),
        Some(TlsAcceptor::from(config)),
    )
    .await;
}

/// [`serve`] with explicit limits. Tests use short ones.
pub async fn serve_with(
    listener: TcpListener,
    app: Router,
    stop: impl Future<Output = ()>,
    limits: ServeLimits,
) {
    serve_with_acceptor(listener, app, stop, limits, None).await;
}

async fn serve_with_acceptor(
    listener: TcpListener,
    app: Router,
    stop: impl Future<Output = ()>,
    limits: ServeLimits,
    acceptor: Option<TlsAcceptor>,
) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let peers = PeerSessions::default();
    peers.set_max(limits.per_address);
    let mut stop = std::pin::pin!(stop);
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let permit = tokio::select! {
            () = &mut stop => break,
            permit = Arc::clone(&slots).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => break,
            },
        };
        let (stream, peer) = tokio::select! {
            () = &mut stop => break,
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    tracing::debug!(%error, "http accept failed");
                    continue;
                }
            },
        };
        // One address cannot hold every slot; its extra connections close.
        let Some(slot) = peers.acquire(peer.ip()) else {
            drop(stream);
            continue;
        };
        let service = app
            .clone()
            .layer(axum::Extension(ConnectInfo(peer)))
            .map_request(|request: Request<hyper::body::Incoming>| request.map(Body::new));
        let acceptor = acceptor.clone();
        connections.spawn(async move {
            if let Some(acceptor) = acceptor {
                match tokio::time::timeout(limits.header_timeout, acceptor.accept(stream)).await {
                    Ok(Ok(stream)) => drive(stream, service, limits).await,
                    Ok(Err(error)) => tracing::debug!(%error, "tls handshake rejected"),
                    Err(_) => tracing::debug!("tls handshake timed out"),
                }
            } else {
                drive(stream, service, limits).await;
            }
            drop(slot);
            drop(permit);
        });
        while connections.try_join_next().is_some() {}
    }
    let _ = tokio::time::timeout(Duration::from_millis(200), async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    connections.shutdown().await;
}

/// Runs one connection until it ends, goes idle, or reaches its maximum age.
/// Idle and aged connections get a graceful shutdown (HTTP/1 finishes the
/// request in flight, HTTP/2 sends GOAWAY), then a hard close after a grace.
async fn drive<I, S>(stream: I, service: S, limits: ServeLimits)
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    S: tower::Service<Request<hyper::body::Incoming>, Response = Response> + Clone + Send,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    S: 'static,
{
    let activity = Arc::new(Activity::new());
    let io = TokioIo::new(Tracked {
        inner: stream,
        activity: Arc::clone(&activity),
    });
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_timeout);
    builder.http2().timer(TokioTimer::new());
    let connection = builder.serve_connection(io, TowerToHyperService::new(service));
    let mut connection = std::pin::pin!(connection);
    let born = Instant::now();
    let period = (limits.idle_timeout / 4).clamp(Duration::from_millis(10), Duration::from_secs(1));
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut hard_close: Option<Instant> = None;
    loop {
        tokio::select! {
            result = connection.as_mut() => {
                if let Err(error) = result {
                    tracing::debug!(%error, "http connection ended");
                }
                return;
            }
            _ = tick.tick() => {
                let now = Instant::now();
                if let Some(at) = hard_close {
                    if now >= at {
                        return;
                    }
                    continue;
                }
                let idle = activity.idle_for() >= limits.idle_timeout;
                let aged = now.duration_since(born) >= limits.max_age;
                if idle || aged {
                    connection.as_mut().graceful_shutdown();
                    let grace = if idle { limits.header_timeout } else { TRANSFER_TIMEOUT };
                    hard_close = Some(now + grace);
                }
            }
        }
    }
}

/// When a connection last moved a byte.
struct Activity {
    start: Instant,
    last_ms: AtomicU64,
}

impl Activity {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            last_ms: AtomicU64::new(0),
        }
    }

    fn touch(&self) {
        let ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_ms.store(ms, Ordering::Relaxed);
    }

    fn idle_for(&self) -> Duration {
        let last = Duration::from_millis(self.last_ms.load(Ordering::Relaxed));
        self.start.elapsed().saturating_sub(last)
    }
}

/// A TCP stream that records every read or write of at least one byte.
struct Tracked<I> {
    inner: I,
    activity: Arc<Activity>,
}

impl<I: AsyncRead + Unpin> AsyncRead for Tracked<I> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.activity.touch();
        }
        polled
    }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for Tracked<I> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, data);
        if matches!(polled, Poll::Ready(Ok(written)) if written > 0) {
            self.activity.touch();
        }
        polled
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CONTENT_SECURITY_POLICY, CSP),
        (header::CACHE_CONTROL, "no-store"),
    ] {
        if !headers.contains_key(&name) {
            headers.insert(name, HeaderValue::from_static(value));
        }
    }
    response
}

async fn per_address(
    State(limiter): State<Arc<KeyedLimiter<IpAddr>>>,
    request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |info| info.0.ip());
    match limiter.check(&crate::limits::peer_key(peer)) {
        Ok(()) => next.run(request).await,
        Err(wait) => too_many_requests(wait),
    }
}

async fn deadline(request: Request, next: Next) -> Response {
    let limit = if request.uri().path().starts_with(ATTACHMENTS) {
        TRANSFER_TIMEOUT
    } else {
        REQUEST_TIMEOUT
    };
    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (
            StatusCode::REQUEST_TIMEOUT,
            Json(json!({ "error": "request timed out" })),
        )
            .into_response(),
    }
}
