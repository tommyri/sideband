//! Review saved server identities without exposing account credentials to the webview.
use crate::{
    commands::{api_error, parse_server_id},
    profile,
    state::AppState,
    types::ApiError,
};
use serde::Serialize;
use ss_client_core::{
    Client, Credentials,
    profile::{ProfileCredentials, account_link::LinkState},
};
use std::sync::Arc;
use tauri::State;
use ts_rs::TS;

#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum AccountLinkStatus {
    Eligible,
    Linked,
    ServerSignInOnly,
    OtherProfile,
    UnauthorizedDevice,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct AccountLinkReview {
    status: AccountLinkStatus,
    user_id: Option<String>,
    username: Option<String>,
    role: Option<String>,
}

fn connected(state: &AppState, id: &str) -> Result<Arc<Client>, ApiError> {
    state
        .client(&parse_server_id(id)?)
        .filter(|c| c.state().read().connection == ss_client_core::ConnectionState::Connected)
        .ok_or_else(|| {
            ApiError::new(
                "disconnected",
                "Sign in to this server to review its account.",
            )
        })
}

fn supports_linking(client: &Client) -> bool {
    client
        .state()
        .read()
        .server
        .as_ref()
        .is_some_and(|s| s.features.iter().any(|f| f == "profileAccountLinking"))
}

async fn keys(state: &AppState, client: &Client) -> Result<Box<ProfileCredentials>, ApiError> {
    let credentials = profile::credentials(
        state.profile_path.clone(),
        client.address().id().map_err(api_error)?,
        String::new(),
        String::new(),
    )
    .await?;
    match credentials {
        Credentials::Profile(keys) => Ok(keys),
        _ => Err(ApiError::new(
            "profile_unavailable",
            "Could not open local profile keys.",
        )),
    }
}

#[tauri::command]
pub async fn review_profile_account(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<AccountLinkReview, ApiError> {
    let client = connected(&state, &server_id)?;
    if !supports_linking(&client) {
        return Ok(AccountLinkReview {
            status: AccountLinkStatus::ServerSignInOnly,
            user_id: None,
            username: None,
            role: None,
        });
    }
    let keys = keys(&state, &client).await?;
    let review = client.review_profile_link(&keys).await.map_err(api_error)?;
    let id = review.id.to_string();
    let status = match review.state {
        LinkState::Eligible => AccountLinkStatus::Eligible,
        LinkState::Linked => {
            // A prior request may have committed remotely just before a crash.
            // Offer confirmation again until this local catalog also records it.
            let recorded = profile::catalog::memberships(&state.profile_path)?
                .iter()
                .any(|entry| {
                    entry.host == client.address().host
                        && entry.port == client.address().port
                        && entry.user_id == id
                });
            if recorded {
                AccountLinkStatus::Linked
            } else {
                AccountLinkStatus::Eligible
            }
        }
        LinkState::OtherProfile => AccountLinkStatus::OtherProfile,
        LinkState::UnauthorizedDevice => AccountLinkStatus::UnauthorizedDevice,
    };
    Ok(AccountLinkReview {
        status,
        user_id: Some(id),
        username: Some(review.username),
        role: Some(review.role),
    })
}

#[tauri::command]
pub async fn link_profile_account(
    state: State<'_, AppState>,
    server_id: String,
    user_id: String,
    role: String,
) -> Result<(), ApiError> {
    let _operation = state.operations.lock().await;
    let client = connected(&state, &server_id)?;
    if !supports_linking(&client) {
        return Err(ApiError::new(
            "unsupported",
            "This server supports per-device sign-in only.",
        ));
    }
    let member = user_id
        .parse()
        .map_err(|_| ApiError::new("invalid_member", "Review this account again."))?;
    let keys = keys(&state, &client).await?;
    client
        .link_profile_account(&keys, member, &role)
        .await
        .map_err(api_error)?;
    profile::catalog::remember_client(&state, &client).await
}
