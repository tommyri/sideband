//! Loopback `/healthz` and `/metrics`.
//!
//! These routes are not on the public HTTP listener. `/metrics` is Prometheus
//! text sampled from the live server.

use std::sync::Arc;

use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use metrics_exporter_prometheus::PrometheusHandle;

use crate::{connection::Server, observe};

/// Health and metrics. Bind this on the internal address only.
pub fn router(server: Arc<Server>, metrics: PrometheusHandle) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics_text))
        .with_state(Arc::new(InternalState { server, metrics }))
}

struct InternalState {
    server: Arc<Server>,
    metrics: PrometheusHandle,
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn metrics_text(State(state): State<Arc<InternalState>>) -> impl IntoResponse {
    sample(&state.server);
    state.metrics.render()
}

fn sample(server: &Server) {
    metrics::describe_gauge!("socialspeak_connections", "Live QUIC sessions");
    metrics::describe_gauge!("socialspeak_voice_members", "Members sitting in voice");
    metrics::describe_counter!(
        "socialspeak_datagrams_forwarded_total",
        "Datagrams accepted by a recipient"
    );
    metrics::describe_counter!(
        "socialspeak_datagrams_dropped_total",
        "Datagrams dropped before a recipient accepted them"
    );
    metrics::describe_counter!(
        "socialspeak_datagrams_received_total",
        "Datagrams read from senders"
    );
    metrics::describe_counter!(
        "socialspeak_datagrams_rejected_total",
        "Datagrams refused: bad header, wrong track, muted, or not seated"
    );
    metrics::describe_counter!(
        "socialspeak_datagrams_rate_limited_total",
        "Sender datagrams dropped for exceeding the bitrate budget"
    );
    metrics::describe_counter!("socialspeak_messages_total", "Messages inserted");
    metrics::describe_counter!(
        "socialspeak_sessions_total",
        "Authenticated sessions opened"
    );
    metrics::describe_counter!(
        "socialspeak_slow_disconnects_total",
        "Connections closed because the control consumer fell behind"
    );
    metrics::describe_histogram!(
        "socialspeak_db_seconds",
        "Time a writer held the SQLite lock"
    );
    metrics::gauge!("socialspeak_connections").set(as_f64(server.registry().connection_count()));
    metrics::gauge!("socialspeak_voice_members").set(as_f64(server.voice_members()));
    let video = server.stream_stats();
    metrics::gauge!("socialspeak_video_tracks").set(as_f64(video.active));
    metrics::counter!("socialspeak_video_bytes_forwarded_total").absolute(video.bytes_forwarded);
    metrics::counter!("socialspeak_video_resets_total").absolute(video.resets);
    metrics::counter!("socialspeak_video_rejected_total").absolute(video.rejected);
    let stats = server.media_stats();
    metrics::counter!("socialspeak_datagrams_forwarded_total").absolute(stats.forwarded);
    metrics::counter!("socialspeak_datagrams_dropped_total").absolute(stats.dropped);
    metrics::counter!("socialspeak_datagrams_received_total").absolute(stats.received);
    metrics::counter!("socialspeak_datagrams_rejected_total").absolute(stats.rejected);
    metrics::counter!("socialspeak_datagrams_rate_limited_total").absolute(stats.rate_limited);
    metrics::counter!("socialspeak_messages_total").absolute(observe::messages());
    metrics::counter!("socialspeak_sessions_total").absolute(observe::sessions());
    metrics::counter!("socialspeak_slow_disconnects_total").absolute(observe::slow_disconnects());
}

fn as_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}
