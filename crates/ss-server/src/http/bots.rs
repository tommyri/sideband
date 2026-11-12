//! The bot API over HTTPS (T-080): actions, reads, and the event stream.
//!
//! Every route takes `Authorization: Bot <token>`. A session bearer is
//! refused, and a bot token is never accepted where a session bearer is, so
//! neither credential stands in for the other.
//!
//! Bodies and replies are the canonical protobuf JSON of the `.proto`
//! messages (`docs/protocol/messages.md`): an action's body is its command
//! (`SendMessage`, `EditMessage`, `CreateDm`) with the ids from the path,
//! and its reply is the `Ack` result, or `204` when that is `Empty`. Ids in
//! paths are ULIDs or base64url. Actions run the QUIC command path
//! ([`Server::bot_command`]), so permissions, scopes, quotas and events are
//! the same. `GET /api/v1/events` is a server-sent event stream: one
//! `data:` line per `ServerEnvelope`, `Welcome` first.

use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{delete, get, patch, post},
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use ss_core::{Id, UserKind};
use ss_proto::{
    v1::{self, ack, client_envelope::Msg, server_envelope},
    validate::id_to_bytes,
};

use crate::{
    auth,
    connection::{Server, StreamFilter},
    db::{self, EVERYONE_ROLE_ID, roles, users},
    handlers::bots::StreamRefused,
    snapshot,
};

/// Longest `prefix` filter on the event stream, in characters.
const MAX_PREFIX: usize = 32;
/// Largest `GET /api/v1/members` page.
const MAX_MEMBERS: usize = 1000;
/// Comment line sent while a stream is quiet, under the idle timeout.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// Bot routes. Mount them on the public HTTPS listener.
pub fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/api/v1/channels/{channel_id}/messages", post(send))
        .route("/api/v1/messages/{message_id}", patch(edit))
        .route("/api/v1/messages/{message_id}", delete(remove))
        .route("/api/v1/dms", post(open_dm))
        .route("/api/v1/members", get(members))
        .route("/api/v1/channels", get(channels))
        .route("/api/v1/events", get(events))
        .with_state(server)
}

async fn send(
    State(server): State<Arc<Server>>,
    Path(channel_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let bot = match caller(&server, &headers).await {
        Ok(bot) => bot,
        Err(response) => return *response,
    };
    let Ok(channel_id) = parse_id(&channel_id) else {
        return bad_request("channel id must be a ULID or base64url");
    };
    let mut command: v1::SendMessage = match parse_body(&headers, &body) {
        Ok(command) => command,
        Err(response) => return *response,
    };
    command.channel_id = id_to_bytes(channel_id);
    answer(server.bot_command(&bot, Msg::SendMessage(command)).await)
}

async fn edit(
    State(server): State<Arc<Server>>,
    Path(message_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let bot = match caller(&server, &headers).await {
        Ok(bot) => bot,
        Err(response) => return *response,
    };
    let Ok(message_id) = parse_id(&message_id) else {
        return bad_request("message id must be a ULID or base64url");
    };
    let mut command: v1::EditMessage = match parse_body(&headers, &body) {
        Ok(command) => command,
        Err(response) => return *response,
    };
    command.message_id = id_to_bytes(message_id);
    answer(server.bot_command(&bot, Msg::EditMessage(command)).await)
}

async fn remove(
    State(server): State<Arc<Server>>,
    Path(message_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let bot = match caller(&server, &headers).await {
        Ok(bot) => bot,
        Err(response) => return *response,
    };
    let Ok(message_id) = parse_id(&message_id) else {
        return bad_request("message id must be a ULID or base64url");
    };
    let command = v1::DeleteMessage {
        message_id: id_to_bytes(message_id),
    };
    answer(server.bot_command(&bot, Msg::DeleteMessage(command)).await)
}

async fn open_dm(State(server): State<Arc<Server>>, headers: HeaderMap, body: Bytes) -> Response {
    let bot = match caller(&server, &headers).await {
        Ok(bot) => bot,
        Err(response) => return *response,
    };
    let command: v1::CreateDm = match parse_body(&headers, &body) {
        Ok(command) => command,
        Err(response) => return *response,
    };
    answer(server.bot_command(&bot, Msg::CreateDm(command)).await)
}

#[derive(Deserialize)]
struct MemberQuery {
    after: Option<String>,
    limit: Option<usize>,
}

/// Members, as in `Snapshot.members`, ascending by user id. Every signed-in
/// account sees the whole member list in its `Welcome`; this pages it.
async fn members(
    State(server): State<Arc<Server>>,
    Query(query): Query<MemberQuery>,
    headers: HeaderMap,
) -> Response {
    let bot = match caller(&server, &headers).await {
        Ok(bot) => bot,
        Err(response) => return *response,
    };
    if let Err(wait) = server.spend_bot(bot.user.id) {
        return rate_limited("bot is sending too many requests", wait);
    }
    let Ok(after) = query.after.as_deref().map(parse_id).transpose() else {
        return bad_request("after must be a ULID or base64url");
    };
    let limit = query.limit.unwrap_or(MAX_MEMBERS);
    if limit == 0 || limit > MAX_MEMBERS {
        return bad_request("limit must be 1 to 1000");
    }
    match member_page(&server, after, limit).await {
        Ok(page) => Json(page).into_response(),
        Err(error) => {
            tracing::debug!(%error, "member page failed");
            internal()
        }
    }
}

async fn member_page(server: &Server, after: Option<Id>, limit: usize) -> Result<Value, db::Error> {
    let mut conn = server.permissions().pool().read().await?;
    let mut accounts = users::list(&mut conn).await?;
    accounts.sort_by_key(|user| user.id);
    accounts.retain(|user| after.is_none_or(|after| user.id > after));
    let more = accounts.len() > limit;
    accounts.truncate(limit);
    let mut grants: std::collections::HashMap<Id, Vec<Id>> = std::collections::HashMap::new();
    for (user, role) in roles::grants(&mut conn).await? {
        grants.entry(user).or_default().push(role);
    }
    let mut members = Vec::with_capacity(accounts.len());
    for account in &accounts {
        let mut role_ids = grants.remove(&account.id).unwrap_or_default();
        if !role_ids.contains(&EVERYONE_ROLE_ID) {
            role_ids.push(EVERYONE_ROLE_ID);
        }
        role_ids.sort_unstable();
        let member = v1::Member {
            user: Some(wire_user(account)),
            roles: role_ids.into_iter().map(id_to_bytes).collect(),
            presence: Some(server.registry().presence(account.id)),
        };
        members.push(to_json(&member)?);
    }
    let mut page = json!({ "members": members });
    if more && let Some(last) = accounts.last() {
        page["next"] = json!(last.id.to_string());
    }
    Ok(page)
}

fn wire_user(user: &db::User) -> v1::User {
    let kind = match user.kind {
        UserKind::Bot => v1::UserKind::Bot,
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

/// Guild channels the bot can view, with its effective permissions, and its
/// most recently active DMs.
async fn channels(State(server): State<Arc<Server>>, headers: HeaderMap) -> Response {
    let bot = match caller(&server, &headers).await {
        Ok(bot) => bot,
        Err(response) => return *response,
    };
    if let Err(wait) = server.spend_bot(bot.user.id) {
        return rate_limited("bot is sending too many requests", wait);
    }
    match channel_list(&server, bot.user.id).await {
        Ok(page) => Json(page).into_response(),
        Err(error) => {
            tracing::debug!(%error, "channel list failed");
            internal()
        }
    }
}

async fn channel_list(server: &Server, bot: Id) -> Result<Value, db::Error> {
    let perms = server.permissions();
    let guild = snapshot::channel_view(perms, bot).await?;
    let direct = {
        let mut conn = perms.pool().read().await?;
        snapshot::direct_page(perms, &mut conn, bot, None, crate::limits::WELCOME_DMS).await?
    };
    let channels = guild.values().map(to_json).collect::<Result<Vec<_>, _>>()?;
    let direct = direct
        .channels
        .iter()
        .map(to_json)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "channels": channels, "directMessages": direct }))
}

#[derive(Deserialize)]
struct EventQuery {
    prefix: Option<String>,
}

/// `text/event-stream` of `ServerEnvelope` JSON, `Welcome` first. The
/// stream ends on revoke (`sessionRevoked`), on falling behind or at its
/// lifetime (`serverNotice`); reconnect after the last two.
async fn events(
    State(server): State<Arc<Server>>,
    Query(query): Query<EventQuery>,
    headers: HeaderMap,
) -> Response {
    let bot = match caller(&server, &headers).await {
        Ok(bot) => bot,
        Err(response) => return *response,
    };
    if let Err(wait) = server.spend_bot(bot.user.id) {
        return rate_limited("bot is sending too many requests", wait);
    }
    if query
        .prefix
        .as_ref()
        .is_some_and(|prefix| prefix.is_empty() || prefix.chars().count() > MAX_PREFIX)
    {
        return bad_request("prefix must be 1 to 32 characters");
    }
    let filter = StreamFilter {
        prefix: query.prefix,
    };
    let frames = match server.bot_events(bot, filter).await {
        Ok(frames) => frames,
        Err(StreamRefused::Busy) => {
            return rate_limited(
                "too many event streams for this bot",
                Duration::from_secs(1),
            );
        }
        // Revoked after this request signed in: no `Welcome` goes out.
        Err(StreamRefused::Revoked) => return unauthorized(),
        Err(StreamRefused::Unavailable) => {
            let mut response = (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "could not confirm the bot token; try again" })),
            )
                .into_response();
            retry_after(&mut response, Duration::from_secs(1));
            return response;
        }
    };
    let stream = futures::stream::unfold(frames, |mut frames| async move {
        let frame = frames.recv().await?;
        let data = serde_json::to_string(&frame).ok()?;
        Some((Ok::<_, Infallible>(Event::default().data(data)), frames))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
        .into_response()
}

/// The bot behind `Authorization: Bot <token>`. A `Bearer` session token is
/// `401`, as is anything else.
async fn caller(
    server: &Server,
    headers: &HeaderMap,
) -> Result<auth::Authenticated, Box<Response>> {
    let Some(token) = bot_token(headers) else {
        return Err(Box::new(unauthorized()));
    };
    match server.bot_caller(token).await {
        Ok(bot) => Ok(bot),
        Err(auth::Error::Unauthenticated | auth::Error::Disabled | auth::Error::Banned { .. }) => {
            Err(Box::new(unauthorized()))
        }
        Err(error) => {
            tracing::debug!(%error, "bot authentication failed");
            Err(Box::new(internal()))
        }
    }
}

fn bot_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bot ")?;
    if token.is_empty() { None } else { Some(token) }
}

/// A ULID, or the 16 id bytes in base64url (the stream's ids, URL-safe).
fn parse_id(text: &str) -> Result<Id, ()> {
    if let Ok(id) = text.parse() {
        return Ok(id);
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| ())?;
    Id::try_from(bytes.as_slice()).map_err(|_| ())
}

fn parse_body<T: serde::de::DeserializeOwned>(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<T, Box<Response>> {
    if !crate::http::hardening::has_content_type(headers, "application/json") {
        return Err(Box::new(
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(json!({ "error": "content type must be application/json" })),
            )
                .into_response(),
        ));
    }
    serde_json::from_slice(body)
        .map_err(|_| Box::new(bad_request("body is not valid JSON for this command")))
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, db::Error> {
    serde_json::to_value(value).map_err(|_| db::Error::Corrupt("json"))
}

/// The HTTP form of a command's `Ack` or `Error`.
fn answer(envelope: v1::ServerEnvelope) -> Response {
    match envelope.msg {
        Some(server_envelope::Msg::Ack(v1::Ack {
            result: Some(ack::Result::Empty(_)) | None,
            ..
        })) => StatusCode::NO_CONTENT.into_response(),
        Some(server_envelope::Msg::Ack(v1::Ack {
            result: Some(result),
            ..
        })) => match to_json(&v1::Ack {
            request_id: 0,
            result: Some(result),
        }) {
            Ok(value) => Json(value).into_response(),
            Err(_) => internal(),
        },
        Some(server_envelope::Msg::Error(error)) => failure(&error),
        _ => internal(),
    }
}

fn failure(error: &v1::Error) -> Response {
    let code = v1::ErrorCode::try_from(error.code).unwrap_or(v1::ErrorCode::Internal);
    let status = match code {
        v1::ErrorCode::Unauthenticated => StatusCode::UNAUTHORIZED,
        v1::ErrorCode::Forbidden | v1::ErrorCode::Banned => StatusCode::FORBIDDEN,
        v1::ErrorCode::NotFound => StatusCode::NOT_FOUND,
        v1::ErrorCode::Invalid | v1::ErrorCode::ProtocolVersion => StatusCode::BAD_REQUEST,
        v1::ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        v1::ErrorCode::Conflict => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let mut response = (
        status,
        Json(json!({ "error": error.message, "code": code.as_str_name() })),
    )
        .into_response();
    if let Some(wait) = error.retry_after_ms {
        retry_after(&mut response, Duration::from_millis(u64::from(wait)));
    }
    response
}

fn rate_limited(message: &str, wait: Duration) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "error": message, "code": v1::ErrorCode::RateLimited.as_str_name() })),
    )
        .into_response();
    retry_after(&mut response, wait);
    response
}

fn retry_after(response: &mut Response, wait: Duration) {
    let seconds = wait.as_millis().div_ceil(1000).max(1);
    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
}

fn unauthorized() -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "a bot token is required: Authorization: Bot <token>" })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bot"));
    response
}

fn bad_request(message: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

fn internal() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal error" })),
    )
        .into_response()
}
