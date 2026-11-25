//! Choose a relay from this profile's authenticated, joined communities.
use super::{
    catalog::{self, Membership, PortableProfile},
    membership_root, record, storage_error,
};
use crate::{
    commands::{api_error, parse_server_id},
    onboarding,
    state::AppState,
    types::{ApiError, ProbeInput},
};
use serde::{Deserialize, Serialize};
use ss_client_core::{Client, profile_sync::RelayKey};
use ss_net::pins::{JsonPinStore, ServerId};
use std::{fs, io::ErrorKind, path::Path, sync::Arc};
use tauri::State;
use tokio::task::JoinSet;
use ts_rs::TS;

#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct SyncServer {
    id: String,
    name: String,
    target: ProbeInput,
    has_public_icon: bool,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct SyncOptions {
    chosen: Option<SyncServer>,
    available: Vec<SyncServer>,
    unchecked: usize,
}

#[derive(Serialize, Deserialize)]
struct Choice {
    key_id: String,
    server: SyncServer,
}

fn chosen(path: &Path, key_id: &str) -> Result<Option<SyncServer>, ApiError> {
    match fs::File::open(path.with_file_name("profile-relay.json")) {
        Ok(file) => {
            let choice: Choice = serde_json::from_reader(file).map_err(storage_error)?;
            Ok((choice.key_id == key_id).then_some(choice.server))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(storage_error(e)),
    }
}

fn save_choice(path: &Path, choice: &Choice) -> Result<(), ApiError> {
    let parent = path.parent().ok_or_else(|| storage_error("profile path"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(storage_error)?;
    serde_json::to_writer(file.as_file_mut(), choice).map_err(storage_error)?;
    file.as_file().sync_all().map_err(storage_error)?;
    file.persist(path.with_file_name("profile-relay.json"))
        .map_err(storage_error)?;
    Ok(())
}

fn unavailable() -> ApiError {
    ApiError::new(
        "relay_unavailable",
        "Reconnect to the server with your profile and check again.",
    )
}

async fn candidate(
    member: Membership,
    client: Arc<Client>,
    pins: Arc<JsonPinStore>,
    key: &RelayKey,
) -> Result<Option<SyncServer>, ApiError> {
    let target = ProbeInput {
        host: member.host,
        port: member.port,
        https_port: member.https_port,
    };
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        onboarding::probe(target.clone(), pins),
    )
    .await
    .map_err(|_| unavailable())??;
    if !probe.trusted || !probe.capabilities.iter().any(|cap| cap == "profileSync") {
        return Ok(None);
    }
    if !client.relay_eligible(key).await.map_err(api_error)? {
        return Ok(None);
    }
    Ok(Some(SyncServer {
        id: client.address().id().map_err(api_error)?.as_str().into(),
        name: probe.name,
        target,
        has_public_icon: probe.has_public_icon,
    }))
}

#[tauri::command]
pub async fn sync_server_list(state: State<'_, AppState>) -> Result<SyncOptions, ApiError> {
    let path = state.profile_path.clone();
    let (members, root, chosen) = tokio::task::spawn_blocking(move || {
        let record = record(&path)?.ok_or_else(catalog::missing)?;
        Ok::<_, ApiError>((
            record.memberships,
            membership_root(&path)?,
            chosen(&path, &record.key_id)?,
        ))
    })
    .await
    .map_err(storage_error)??;
    let mut options = SyncOptions {
        chosen,
        available: vec![],
        unchecked: 0,
    };
    let mut members = members.into_iter();
    let mut running = JoinSet::new();
    loop {
        while running.len() < 4 {
            let Some(member) = members.next() else {
                break;
            };
            let id = member.server_id()?;
            let Some(client) = state.client(&id) else {
                options.unchecked += 1;
                continue;
            };
            let key = RelayKey::new(&root, id).map_err(api_error)?;
            let pins = state.pins.clone();
            running.spawn(async move { candidate(member, client, pins, &key).await });
        }
        match running.join_next().await {
            Some(Ok(Ok(Some(server)))) => options.available.push(server),
            Some(Ok(Ok(None))) => {}
            Some(_) => options.unchecked += 1,
            None => break,
        }
    }
    options
        .available
        .sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    Ok(options)
}

fn selected_membership(profile: &PortableProfile, id: &ServerId) -> Result<Membership, ApiError> {
    profile
        .memberships
        .iter()
        .find(|m| m.server_id().as_ref().ok() == Some(id))
        .cloned()
        .ok_or_else(unavailable)
}

fn verified_copy(key: &RelayKey, ciphertext: &mut [u8]) -> Result<PortableProfile, ApiError> {
    let plaintext = key.open(ciphertext).map_err(api_error)?;
    let profile: PortableProfile = serde_json::from_slice(plaintext).map_err(|_| {
        ApiError::new(
            "invalid_copy",
            "The server's encrypted copy could not be read.",
        )
    })?;
    profile.validate()?;
    Ok(profile)
}

async fn prepare_copy(
    client: &Client,
    key: &RelayKey,
    profile: &PortableProfile,
) -> Result<(), ApiError> {
    let current = client.relay_copy(key).await.map_err(api_error)?;
    if let Some(mut current) = current {
        let saved = verified_copy(key, &mut current.ciphertext)?;
        if &saved == profile {
            return Ok(());
        }
        // An interrupted choice can be retried even if more memberships were
        // joined locally. Preserve a divergent or newer remote copy for reconciliation.
        if saved.name != profile.name
            || !saved
                .memberships
                .iter()
                .all(|m| profile.memberships.contains(m))
        {
            return Err(ApiError::new(
                "copy_conflict",
                "This server already holds a different profile update. Its copy has been kept.",
            ));
        }
        let bytes = key
            .seal(&serde_json::to_vec(profile).map_err(storage_error)?)
            .map_err(api_error)?;
        return client
            .write_relay_copy(key, Some(&current.etag), bytes)
            .await
            .map_err(api_error);
    }
    let bytes = key
        .seal(&serde_json::to_vec(profile).map_err(storage_error)?)
        .map_err(api_error)?;
    client
        .write_relay_copy(key, None, bytes)
        .await
        .map_err(api_error)
}

#[tauri::command]
pub async fn choose_sync_server(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<(), ApiError> {
    let _selection = state.relay_selection.lock().await;
    let id = parse_server_id(&server_id)?;
    let path = state.profile_path.clone();
    let server = id.clone();
    let (profile, key, key_id) = tokio::task::spawn_blocking(move || {
        let record = record(&path)?.ok_or_else(catalog::missing)?;
        if chosen(&path, &record.key_id)?.is_some_and(|c| c.id != server.as_str()) {
            return Err(ApiError::new(
                "relay_chosen",
                "You already chose a sync server.",
            ));
        }
        Ok::<_, ApiError>((
            catalog::snapshot(&path)?,
            RelayKey::new(&*membership_root(&path)?, server).map_err(api_error)?,
            record.key_id,
        ))
    })
    .await
    .map_err(storage_error)??;
    let member = selected_membership(&profile, &id)?;
    let client = state.client(&id).ok_or_else(unavailable)?;
    let server = candidate(member, client.clone(), state.pins.clone(), &key)
        .await?
        .ok_or_else(unavailable)?;
    prepare_copy(&client, &key, &profile).await?;
    // Persist success after the relay acknowledges. Reopening and retrying
    // after an interrupted response authenticates the existing copy first.
    let _write = state.operations.lock().await;
    let path = state.profile_path.clone();
    tokio::task::spawn_blocking(move || {
        let current = record(&path)?.ok_or_else(catalog::missing)?;
        if current.key_id != key_id
            || !current
                .memberships
                .iter()
                .any(|m| m.server_id().ok() == Some(id.clone()))
        {
            return Err(unavailable());
        }
        if chosen(&path, &key_id)?.is_some_and(|c| c.id != server.id) {
            return Err(ApiError::new(
                "relay_chosen",
                "Another sync server was chosen.",
            ));
        }
        save_choice(&path, &Choice { key_id, server })
    })
    .await
    .map_err(storage_error)?
}

#[tauri::command]
pub async fn check_sync_copy(state: State<'_, AppState>) -> Result<(), ApiError> {
    let path = state.profile_path.clone();
    let (id, key) = tokio::task::spawn_blocking(move || {
        let record = record(&path)?.ok_or_else(catalog::missing)?;
        let choice = chosen(&path, &record.key_id)?.ok_or_else(unavailable)?;
        let id = parse_server_id(&choice.id)?;
        let key = RelayKey::new(&*membership_root(&path)?, id.clone()).map_err(api_error)?;
        Ok::<_, ApiError>((id, key))
    })
    .await
    .map_err(storage_error)??;
    let client = state.client(&id).ok_or_else(unavailable)?;
    let mut copy = client
        .relay_copy(&key)
        .await
        .map_err(api_error)?
        .ok_or_else(|| {
            ApiError::new(
                "copy_missing",
                "The sync server has no saved copy. Save a new encrypted copy.",
            )
        })?;
    let profile = verified_copy(&key, &mut copy.ciphertext)?;
    selected_membership(&profile, &id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acknowledged_choice_reopens_only_with_the_same_local_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        let server = SyncServer {
            id: "[room.example]:443".into(),
            name: "Friday Games".into(),
            target: ProbeInput {
                host: "room.example".into(),
                port: 443,
                https_port: 8443,
            },
            has_public_icon: false,
        };
        assert!(chosen(&path, "vault-a").unwrap().is_none());
        save_choice(
            &path,
            &Choice {
                key_id: "vault-a".into(),
                server,
            },
        )
        .unwrap();
        let selected = chosen(&path, "vault-a").unwrap().unwrap();
        assert_eq!(selected.target.https_port, 8443);
        assert_eq!(selected.name, "Friday Games");
        assert!(chosen(&path, "vault-b").unwrap().is_none());
    }
}
