use crate::{
    state::AppState,
    types::{ApiError, ProbeInput, ServerProbe},
};
use base64::Engine as _;
use ss_client_core::storage::Storage as _;
use ss_net::{
    pins::{MemoryPinStore, PinStore, ServerId, TrustPolicy},
    tls::Fingerprint,
};
use std::{sync::Arc, time::Duration};
use tauri::State;

/// Discover the actual QUIC certificate without sending a Hello or credentials.
/// Metadata uses domain validation or the observed pin, never trust-all HTTPS.
async fn probe(input: ProbeInput, pins: Arc<dyn PinStore>) -> Result<ServerProbe, ApiError> {
    probe_with_policy(input, pins, false).await
}

/// Server metadata from `GET /api/v1/server`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    name: String,
    registration_mode: String,
    fingerprint: String,
    #[serde(default)]
    capabilities: Vec<String>,
    icon_url: Option<String>,
}

const PUBLIC_ICON: &str = "/api/v1/server/icon";
const MAX_ICON: usize = 2 * 1024 * 1024;

/// Connect once over QUIC and report the certificate the server presented
/// and the trust that would apply to it.
async fn quic_trust(
    address: &ss_client_core::ServerAddr,
    pins: Arc<MemoryPinStore>,
) -> Result<(Fingerprint, TrustPolicy), ApiError> {
    match ss_net::client::connect(&address.host, address.port, pins).await {
        Ok(connection) => {
            let fingerprint = connection
                .peer_fingerprint()
                .ok_or_else(|| ApiError::new("client", "No server certificate received."))?;
            let policy = connection
                .trust_policy()
                .ok_or_else(|| ApiError::new("client", "Missing verified trust."))?;
            connection.close(0, b"onboarding probe complete");
            Ok((fingerprint, policy))
        }
        Err(ss_net::Error::Untrusted { fingerprint }) => {
            Ok((fingerprint, TrustPolicy::PinnedCertificate { fingerprint }))
        }
        Err(ss_net::Error::FingerprintChanged { received, .. }) => Ok((
            received,
            TrustPolicy::PinnedCertificate {
                fingerprint: received,
            },
        )),
        Err(e) => Err(ApiError::new("certificate_validation", e.to_string())),
    }
}

/// Only request fixed paths at the same validated host, under the QUIC trust policy.
async fn public_response(
    policy: TrustPolicy,
    host: &str,
    https_port: u16,
    path: &'static str,
) -> Result<reqwest::Response, ApiError> {
    let http = reqwest::Client::builder()
        .use_preconfigured_tls(
            ss_net::client::tls_config(policy)
                .map_err(|e| ApiError::new("client", e.to_string()))?,
        )
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(http_error)?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let response = http
        .get(format!("https://{host}:{https_port}{path}"))
        .send()
        .await
        .map_err(http_error)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(http_error(response.status()));
    }
    Ok(response)
}

async fn read_bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, ApiError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(ApiError::new(
                "invalid_server",
                "Server response is too large.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn server_metadata(
    policy: TrustPolicy,
    host: &str,
    https_port: u16,
) -> Result<Metadata, ApiError> {
    let response = public_response(policy, host, https_port, "/api/v1/server").await?;
    serde_json::from_slice(&read_bounded(response, 16 * 1024).await?)
        .map_err(|_| ApiError::new("invalid_server", "Invalid server metadata."))
}

fn recognized_capabilities(capabilities: Vec<String>) -> Vec<String> {
    capabilities
        .into_iter()
        .filter(|s| {
            matches!(
                s.as_str(),
                "profileSignIn" | "linkedDeviceSignIn" | "profileSync"
            )
        })
        .collect()
}

async fn probe_with_policy(
    input: ProbeInput,
    pins: Arc<dyn PinStore>,
    public: bool,
) -> Result<ServerProbe, ApiError> {
    let address = ss_client_core::ServerAddr::new(input.host, input.port)
        .map_err(|_| ApiError::new("invalid_server", "Enter a valid server address."))?;
    if input.port == 0 || input.https_port == 0 {
        return Err(ApiError::new(
            "invalid_server",
            "Ports must be between 1 and 65535.",
        ));
    }
    let id = address.id().map_err(super::commands::api_error)?;
    let previous_policy = pins
        .policy(&id)
        .map_err(|e| super::commands::api_error(e.into()))?;
    let previous = match previous_policy {
        Some(TrustPolicy::PinnedCertificate { fingerprint }) => Some(fingerprint),
        _ => None,
    };
    let temporary = Arc::new(MemoryPinStore::default());
    if let Some(policy) = if public {
        Some(TrustPolicy::PublicCertificate)
    } else {
        previous_policy
    } {
        temporary
            .compare_exchange(&id, None, policy)
            .map_err(|e| super::commands::api_error(e.into()))?;
    }
    let (fingerprint, policy) = quic_trust(&address, temporary.clone()).await?;
    let metadata = server_metadata(policy, &address.host, input.https_port).await?;
    if policy != TrustPolicy::PublicCertificate
        && Fingerprint::try_from(metadata.fingerprint).ok() != Some(fingerprint)
    {
        return Err(ApiError::new(
            "invalid_server",
            "HTTPS metadata and QUIC certificate do not match.",
        ));
    }
    if policy == TrustPolicy::PublicCertificate {
        pins.compare_exchange(&id, previous_policy, policy)
            .map_err(|e| super::commands::api_error(e.into()))?;
    }
    Ok(ServerProbe {
        name: metadata.name,
        capabilities: recognized_capabilities(metadata.capabilities),
        has_public_icon: metadata.icon_url.as_deref() == Some(PUBLIC_ICON),
        registration_mode: metadata.registration_mode,
        fingerprint: fingerprint.to_string(),
        previous_fingerprint: previous.map(|p| p.to_string()),
        trusted: policy == TrustPolicy::PublicCertificate || previous == Some(fingerprint),
        public_certificate: policy == TrustPolicy::PublicCertificate,
    })
}

fn http_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new("unreachable", error.to_string())
}

#[tauri::command]
pub async fn probe_server(
    state: State<'_, AppState>,
    input: ProbeInput,
) -> Result<ServerProbe, ApiError> {
    tokio::time::timeout(Duration::from_secs(12), probe(input, state.pins.clone()))
        .await
        .map_err(|_| ApiError::new("timeout", "The server did not respond."))?
}

/// Load decorative artwork only after the server's trust policy has been accepted.
#[tauri::command]
pub async fn get_public_server_icon(
    state: State<'_, AppState>,
    input: ProbeInput,
) -> Result<String, ApiError> {
    public_server_icon(&input, &*state.pins).await
}

async fn public_server_icon(input: &ProbeInput, pins: &dyn PinStore) -> Result<String, ApiError> {
    let address = ss_client_core::ServerAddr::new(&input.host, input.port)
        .map_err(super::commands::api_error)?;
    let policy = pins
        .policy(&address.id().map_err(super::commands::api_error)?)
        .map_err(|e| super::commands::api_error(e.into()))?
        .ok_or_else(|| ApiError::new("untrusted", "Approve the server certificate first."))?;
    let response = public_response(policy, &address.host, input.https_port, PUBLIC_ICON).await?;
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(image::ImageFormat::from_mime_type)
        .filter(|kind| {
            matches!(
                kind,
                image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP
            )
        })
        .ok_or_else(|| ApiError::new("icon", "Unsupported server logo."))?;
    let bytes = read_bounded(response, MAX_ICON).await?;
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|_| ApiError::new("icon", "Invalid server logo."))?;
    if reader.format() != Some(mime)
        || !reader
            .into_dimensions()
            .is_ok_and(|(w, h)| (1..=4096).contains(&w) && (1..=4096).contains(&h))
    {
        return Err(ApiError::new("icon", "Invalid server logo."));
    }
    Ok(format!(
        "data:{};base64,{}",
        mime.to_mime_type(),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// Explicit migration: verify both transports before replacing a legacy pin.
#[tauri::command]
pub async fn use_public_certificate(
    state: State<'_, AppState>,
    input: ProbeInput,
) -> Result<ServerProbe, ApiError> {
    let _operation = state.operations.lock().await;
    let id =
        ServerId::new(&input.host, input.port).map_err(|e| super::commands::api_error(e.into()))?;
    if state.clients().get(&id).is_some() {
        return Err(ApiError::new(
            "connected",
            "Disconnect this server before changing its trust mode.",
        ));
    }
    tokio::time::timeout(
        Duration::from_secs(12),
        probe_with_policy(input, state.pins.clone(), true),
    )
    .await
    .map_err(|_| ApiError::new("timeout", "The server did not respond."))?
}

#[tauri::command]
pub async fn remove_server(state: State<'_, AppState>, server_id: String) -> Result<(), ApiError> {
    let _operation = state.operations.lock().await;
    crate::voice::stop(&state, Some(&server_id)).await?;
    let saved = state
        .storage
        .servers()
        .into_iter()
        .find(|s| s.id == server_id)
        .ok_or_else(|| ApiError::new("invalid_server", "Unknown server."))?;
    let id = ServerId::new(&saved.host, saved.port)
        .map_err(|e| ApiError::new("invalid_server", e.to_string()))?;
    let removed = state.clients().remove(&id);
    let token = state
        .storage
        .session(&id)
        .map_err(super::commands::api_error)?;
    let address = ss_client_core::ServerAddr {
        https_port: saved.https_port,
        ..ss_client_core::ServerAddr::new(&saved.host, saved.port)
            .map_err(super::commands::api_error)?
    };
    sign_out(removed, address, token, state.pins.clone()).await;
    // Keep the certificate pin: removing a server must not erase protection
    // against an unexpected certificate when it is added again.
    state
        .storage
        .write(move |storage| storage.remove(&server_id))
        .await
        .map_err(super::commands::api_error)
}

/// How long removing a server waits for the server to confirm the logout.
const SIGN_OUT_TIMEOUT: Duration = Duration::from_secs(5);

/// Ends the saved session on the server so the token is useless even if a
/// copy survives on disk, then closes the client. Uses the live client when
/// there is one, otherwise connects once with the saved token. Best effort:
/// an unreachable server never blocks removal, and the token is forgotten
/// locally either way.
async fn sign_out(
    client: Option<Arc<ss_client_core::Client>>,
    address: ss_client_core::ServerAddr,
    token: Option<String>,
    pins: Arc<dyn PinStore>,
) {
    if let Some(client) = client {
        let done = tokio::time::timeout(SIGN_OUT_TIMEOUT, client.logout()).await;
        crate::state::close(client).await;
        if matches!(done, Ok(Ok(()))) {
            return;
        }
    }
    let Some(token) = token else { return };
    let _ = tokio::time::timeout(SIGN_OUT_TIMEOUT, async move {
        // Memory storage: this throwaway connection must not save anything.
        let client = ss_client_core::Client::connect(
            address,
            ss_client_core::Credentials::Session(token),
            pins,
        )
        .await?;
        let result = client.logout().await;
        client.close().await;
        result
    })
    .await;
}

#[tauri::command]
pub async fn reorder_servers(
    state: State<'_, AppState>,
    server_ids: Vec<String>,
) -> Result<(), ApiError> {
    let _operation = state.operations.lock().await;
    state
        .storage
        .write(move |storage| storage.reorder(&server_ids))
        .await
        .map_err(super::commands::api_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::DesktopStorage;
    use ss_client_core::{Client, Credentials, ServerAddr, v1};
    use ss_net::pins::JsonPinStore;

    #[test]
    fn legacy_metadata_and_unknown_capabilities_are_compatible() {
        let mut value = serde_json::json!({
            "name":"Friends", "fingerprint":"ab".repeat(32), "registrationMode":"invite", "version":"old"
        });
        let legacy: Metadata = serde_json::from_value(value.clone()).unwrap();
        assert!(legacy.capabilities.is_empty());
        assert!(legacy.icon_url.is_none());
        assert_eq!(legacy.registration_mode, "invite");
        value["capabilities"] = serde_json::json!(["future", "profileSignIn", "profileSync"]);
        let current: Metadata = serde_json::from_value(value).unwrap();
        assert_eq!(
            recognized_capabilities(current.capabilities),
            ["profileSignIn", "profileSync"]
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end scenario against a real server"
    )]
    #[tokio::test]
    async fn discovery_requires_explicit_trust_then_registration_and_saved_login_work() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            ss_server::db::Pool::open(dir.path().join("test.db"))
                .await
                .unwrap(),
        );
        let invite = ss_server::auth::ensure_owner_invite(&pool)
            .await
            .unwrap()
            .unwrap();
        let identity = ss_server::tls::prepare(&ss_server::config::Config::default(), dir.path())
            .await
            .unwrap();
        let endpoint = ss_net::server::Endpoint::bind_resolver(
            "127.0.0.1:0".parse().unwrap(),
            identity.resolver(),
        )
        .unwrap();
        let server = Arc::new(
            ss_server::connection::Server::open(pool.clone())
                .await
                .unwrap(),
        );
        let quic = tokio::spawn(server.clone().run(endpoint.clone()));
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let input = ProbeInput {
            host: "127.0.0.1".into(),
            port: endpoint.local_addr().unwrap().port(),
            https_port: tcp.local_addr().unwrap().port(),
        };
        let https = tokio::spawn(ss_server::http::hardening::serve_tls(
            tcp,
            ss_server::http::info::router(pool, identity.fingerprint_slot())
                .merge(server.public_routes()),
            identity.https_config(),
            std::future::pending(),
        ));
        let pins = Arc::new(JsonPinStore::open(dir.path().join("pins.json")).unwrap());
        let id = ServerId::new(&input.host, input.port).unwrap();
        let found = probe(input.clone(), pins.clone()).await.unwrap();
        assert!(!found.trusted);
        assert!(!found.has_public_icon);
        assert_eq!(
            public_server_icon(&input, &*pins).await.unwrap_err().code,
            "untrusted"
        );
        assert_eq!(found.registration_mode, "invite");
        assert!(
            pins.get(&id).unwrap().is_none(),
            "discovery must not persist trust"
        );
        let address = ServerAddr {
            host: input.host.clone(),
            port: input.port,
            https_port: input.https_port,
        };
        let storage = Arc::new(DesktopStorage::open(dir.path().join("sessions.json")).unwrap());
        let rejected = Client::connect_with_storage(
            address.clone(),
            Credentials::Login(v1::Login {
                username: "owner".into(),
                password: "not-sent".into(),
            }),
            pins.clone(),
            storage.clone(),
        )
        .await;
        assert!(matches!(
            rejected,
            Err(ss_client_core::Error::Network(
                ss_net::Error::Untrusted { .. }
            ))
        ));
        pins.set(&id, Fingerprint::try_from(found.fingerprint).unwrap())
            .unwrap();
        assert!(probe(input.clone(), pins.clone()).await.unwrap().trusted);
        let old_policy = pins.policy(&id).unwrap();
        assert!(
            probe_with_policy(input.clone(), pins.clone(), true)
                .await
                .is_err()
        );
        assert_eq!(
            pins.policy(&id).unwrap(),
            old_policy,
            "failed public validation must preserve explicit trust"
        );

        let client = Client::connect_with_storage(
            address.clone(),
            Credentials::Register(v1::Register {
                username: "owner".into(),
                password: "test-password".into(),
                invite_code: invite,
            }),
            pins.clone(),
            storage.clone(),
        )
        .await
        .unwrap();
        assert_native_logo(&client, &input, &*pins).await;
        client.close().await;
        let reopened = Arc::new(DesktopStorage::open(dir.path().join("sessions.json")).unwrap());
        let client = Client::connect_with_storage(
            address.clone(),
            Credentials::Saved,
            pins.clone(),
            reopened.clone(),
        )
        .await
        .unwrap();
        // Removing a connected server logs its session out on the server.
        let token = reopened.session(&id).unwrap().unwrap();
        sign_out(
            Some(Arc::new(client)),
            address.clone(),
            Some(token.clone()),
            pins.clone(),
        )
        .await;
        assert_rejected(&address, &token, pins.clone()).await;
        // A disconnected one is logged out with the saved token.
        let client = Client::connect_with_storage(
            address.clone(),
            Credentials::Login(v1::Login {
                username: "owner".into(),
                password: "test-password".into(),
            }),
            pins.clone(),
            reopened.clone(),
        )
        .await
        .unwrap();
        client.close().await;
        let token = reopened.session(&id).unwrap().unwrap();
        sign_out(None, address.clone(), Some(token.clone()), pins.clone()).await;
        assert_rejected(&address, &token, pins.clone()).await;
        let old = Fingerprint::try_from("ab".repeat(32)).unwrap();
        pins.set(&id, old).unwrap();
        assert!(public_server_icon(&input, &*pins).await.is_err());
        let changed = probe(input, pins.clone()).await.unwrap();
        assert!(!changed.trusted);
        assert_eq!(changed.previous_fingerprint, Some(old.to_string()));
        assert_eq!(
            pins.get(&id).unwrap(),
            Some(old),
            "probe must not replace trust"
        );
        quic.abort();
        https.abort();
        endpoint.close(0, b"test complete");
    }

    async fn assert_native_logo(client: &Client, input: &ProbeInput, pins: &dyn PinStore) {
        let mut source = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(2, 2)
            .write_to(&mut source, image::ImageFormat::Png)
            .unwrap();
        let png = source.into_inner();
        let icon = client.upload_server_icon(png.clone()).await.unwrap();
        let mut settings = client.server_settings().await.unwrap().settings;
        settings.icon_id = Some(icon);
        settings.public_icon = Some(true);
        client.save_server_settings(&settings).await.unwrap();
        let logo = public_server_icon(input, pins).await.unwrap();
        assert_eq!(
            logo.strip_prefix("data:image/png;base64,"),
            Some(
                base64::engine::general_purpose::STANDARD
                    .encode(png)
                    .as_str()
            )
        );
    }

    async fn assert_rejected(address: &ServerAddr, token: &str, pins: Arc<JsonPinStore>) {
        let reused =
            Client::connect(address.clone(), Credentials::Session(token.into()), pins).await;
        assert!(
            matches!(
                reused,
                Err(ss_client_core::Error::Server { code, .. })
                    if code == i32::from(v1::ErrorCode::Unauthenticated)
            ),
            "a signed-out token still works"
        );
    }
}
