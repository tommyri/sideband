use crate::{
    state::AppState,
    types::{ApiError, ProbeInput, ServerProbe},
};
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
    let (fingerprint, policy) =
        match ss_net::client::connect(&address.host, address.port, temporary.clone()).await {
            Ok(connection) => {
                let fingerprint = connection
                    .peer_fingerprint()
                    .ok_or_else(|| ApiError::new("client", "No server certificate received."))?;
                let policy = connection
                    .trust_policy()
                    .ok_or_else(|| ApiError::new("client", "Missing verified trust."))?;
                connection.close(0, b"onboarding probe complete");
                (fingerprint, policy)
            }
            Err(ss_net::Error::Untrusted { fingerprint }) => {
                (fingerprint, TrustPolicy::PinnedCertificate { fingerprint })
            }
            Err(ss_net::Error::FingerprintChanged { received, .. }) => (
                received,
                TrustPolicy::PinnedCertificate {
                    fingerprint: received,
                },
            ),
            Err(e) => return Err(ApiError::new("certificate_validation", e.to_string())),
        };
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
    let host = if address.host.contains(':') {
        format!("[{}]", address.host)
    } else {
        address.host
    };
    let mut response = http
        .get(format!("https://{host}:{}/api/v1/server", input.https_port))
        .send()
        .await
        .map_err(http_error)?
        .error_for_status()
        .map_err(http_error)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        if bytes.len() + chunk.len() > 16_384 {
            return Err(ApiError::new(
                "invalid_server",
                "Server metadata is too large.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Metadata {
        name: String,
        registration_mode: String,
        fingerprint: String,
    }
    let metadata: Metadata = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::new("invalid_server", "Invalid server metadata."))?;
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
        registration_mode: metadata.registration_mode,
        fingerprint: fingerprint.to_string(),
        previous_fingerprint: previous.map(|p| p.to_string()),
        trusted: policy == TrustPolicy::PublicCertificate || previous == Some(fingerprint),
        public_certificate: policy == TrustPolicy::PublicCertificate,
    })
}

fn http_error(error: reqwest::Error) -> ApiError {
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

/// Explicit migration: verify both transports before replacing a legacy pin.
#[tauri::command]
pub async fn use_public_certificate(
    state: State<'_, AppState>,
    input: ProbeInput,
) -> Result<ServerProbe, ApiError> {
    let _operation = state.operations.lock().await;
    let id =
        ServerId::new(&input.host, input.port).map_err(|e| super::commands::api_error(e.into()))?;
    if state.clients.lock().await.get(&id).is_some() {
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
    if let Some(client) = state.clients.lock().await.remove(&id) {
        client.close().await;
    }
    // Keep the certificate pin: removing a server must not erase protection
    // against an unexpected certificate when it is added again.
    state
        .storage
        .remove(&server_id)
        .map_err(super::commands::api_error)
}

#[tauri::command]
pub async fn reorder_servers(
    state: State<'_, AppState>,
    server_ids: Vec<String>,
) -> Result<(), ApiError> {
    let _operation = state.operations.lock().await;
    state
        .storage
        .reorder(&server_ids)
        .map_err(super::commands::api_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::DesktopStorage;
    use ss_client_core::{Client, Credentials, ServerAddr, v1};
    use ss_net::pins::JsonPinStore;

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
        let quic = tokio::spawn(server.run(endpoint.clone()));
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let input = ProbeInput {
            host: "127.0.0.1".into(),
            port: endpoint.local_addr().unwrap().port(),
            https_port: tcp.local_addr().unwrap().port(),
        };
        let https = tokio::spawn(ss_server::http::hardening::serve_tls(
            tcp,
            ss_server::http::info::router(pool, identity.fingerprint_slot()),
            identity.https_config(),
            std::future::pending(),
        ));
        let pins = Arc::new(JsonPinStore::open(dir.path().join("pins.json")).unwrap());
        let id = ServerId::new(&input.host, input.port).unwrap();
        let found = probe(input.clone(), pins.clone()).await.unwrap();
        assert!(!found.trusted);
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
        client.close().await;
        let reopened = Arc::new(DesktopStorage::open(dir.path().join("sessions.json")).unwrap());
        let client =
            Client::connect_with_storage(address, Credentials::Saved, pins.clone(), reopened)
                .await
                .unwrap();
        client.close().await;
        let old = Fingerprint::try_from("ab".repeat(32)).unwrap();
        pins.set(&id, old).unwrap();
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
}
